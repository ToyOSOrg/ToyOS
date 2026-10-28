//! A mass-storage interface, SCSI over Bulk-Only Transport 1.0, served as a
//! disk whose work the block service's sessions queue ([`Disks`]).
//!
//! **One command at a time per device**, as Bulk-Only has it (§5.1): a CBW,
//! a data phase moved straight between the device and the client's region,
//! and a CSW held to §6.3 ([`crate::bot`]). Where a command breaks, the
//! ladder answers ([`crate::ladder`]): Reset Recovery (§5.3.4) verified by a
//! TEST UNIT READY under its own tag; then a port reset and enumeration
//! again, the disk held for its device's return ([`crate::identity`]); then
//! offline, the last thing it is sent a reset.
//!
//! **A command's bound is policy.** BOT and SBC give a command no time
//! limit, and a stick has answered later than two seconds
//! (`issues/a-stick-that-answers-late-is-broken-by-the-two-second-abandon.md`):
//! [`COMMAND_NS`].

use alloc::collections::{BTreeMap, VecDeque};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::num::NonZeroU16;

use toyos_device_memory::{DmaBuffers, Registers};
use toyos_scsi::{self as scsi, Cdb, Geometry, Reply, Sense, HOST_BLOCK, SENSE_BYTES};

use crate::bot::{self, Answer, Bot, Broke, Pipe, RoundTrip, CBW_LEN, CSW_LEN};
use crate::configure::Configure;
use crate::descriptor::{Endpoint, Interface, BULK};
use crate::exec::{self, Dead, Host, Nanos};
use crate::flush::Debt;
use crate::hc::{self, Done, Trb, TRB_MOST};
use crate::identity::{self, Identity, Serial, UsbId};
use crate::ladder::{Left, Rung, Run};
use crate::usb::{Ask, Device, Setup, Usb};

/// How long one Bulk-Only transfer may go unanswered before it is taken
/// back. Policy, on the reasoning behind diskserver's own bound: the slowest
/// thing a healthy device does is flush a large cache.
pub const COMMAND_NS: Nanos = 10_000_000_000;
/// How long a device that answers NOT READY is asked again before it is
/// called unready. Policy: a stick spins up in well under it.
const READY_NS: Nanos = 10_000_000_000;
/// Work one disk holds queued at once.
pub const QUEUE: usize = 32;
/// Most READ(10)s of a bring-up's table read before it is refused.
const TABLE_READS: usize = 64;

/// The silence a transfer that went unanswered ended in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Silent;

/// One piece of work a session queued on a disk; `token` is the session's
/// own name for it, answered back.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Work {
    /// `blocks` host blocks at host block `block`, to or from device address
    /// `at`, which the controller's domain maps.
    Io { write: bool, block: u64, blocks: u32, at: u64, token: (u64, u32) },
    /// The device's volatile cache to the medium.
    Flush { token: (u64, u32) },
}

impl Work {
    fn token(&self) -> (u64, u32) {
        match *self {
            Self::Io { token, .. } | Self::Flush { token } => token,
        }
    }
}

/// What happened to a disk, for its sessions.
#[derive(Debug)]
pub enum Event<T> {
    /// A disk came up, and this is its table.
    Arrived { disk: u32, name: String, table: T },
    /// One piece of work was answered; `status` is the sense the device gave
    /// a refusal (key, ASC and ASCQ), or 0.
    Answered { disk: u32, token: (u64, u32), write: bool, ok: bool, status: u32 },
    /// The device came back from a reset that may have lost its cache:
    /// every piece of work queued is dropped unanswered, and the sessions
    /// answer it.
    Reset { disk: u32 },
    /// The disk left: everything queued was answered not done.
    Gone { disk: u32 },
}

struct Disk {
    identity: Identity,
    geometry: Geometry,
    work: VecDeque<Work>,
    /// The device is away, held for its return until then.
    away: Option<Nanos>,
    /// When this driver last reset its port.
    reset_at: Option<Nanos>,
    no_cache: bool,
    debt: Debt,
}

/// Every disk on the controller.
pub struct Disks<T> {
    disks: RefCell<BTreeMap<u32, Disk>>,
    events: RefCell<VecDeque<Event<T>>>,
    next: Cell<u32>,
    /// Mass-storage functions bound and not yet serving or refused.
    binding: Cell<u32>,
    submitted: Cell<u64>,
}

impl<T> Default for Disks<T> {
    fn default() -> Self {
        Self { disks: RefCell::default(), events: RefCell::default(), next: Cell::new(0), binding: Cell::new(0), submitted: Cell::new(0) }
    }
}

/// A mass-storage function counted as not yet settled, until it drops.
pub struct Binding<'d>(&'d Cell<u32>);

impl Drop for Binding<'_> {
    fn drop(&mut self) {
        self.0.set(self.0.get() - 1);
    }
}

impl<T> Disks<T> {
    pub fn binding(&self) -> Binding<'_> {
        self.binding.set(self.binding.get() + 1);
        Binding(&self.binding)
    }

    pub(crate) fn settled(&self) -> bool {
        self.binding.get() == 0
    }

    /// Everything that happened since the last call.
    pub fn take_events(&self) -> Vec<Event<T>> {
        self.events.borrow_mut().drain(..).collect()
    }

    /// Whether disk `disk` takes one more piece of work now.
    pub fn has_room(&self, disk: u32) -> bool {
        self.disks.borrow().get(&disk).is_some_and(|d| d.work.len() < QUEUE)
    }

    /// Whether a flush of disk `disk` is issued at all: a device that said it
    /// has no cache has its writes on the medium once answered.
    pub fn cached(&self, disk: u32) -> bool {
        self.disks.borrow().get(&disk).is_some_and(|d| !d.no_cache)
    }

    /// Queue `work` on disk `disk`, which its sessions were offered and which
    /// has not gone.
    pub fn submit(&self, disk: u32, work: Work) {
        let mut disks = self.disks.borrow_mut();
        let d = disks.get_mut(&disk).unwrap_or_else(|| panic!("usbd: work queued on disk {disk}, which is not served"));
        assert!(d.work.len() < QUEUE, "usbd: work queued on disk {disk} past its room");
        d.work.push_back(work);
        self.submitted.set(self.submitted.get() + 1);
    }

    /// How many pieces of work were ever queued: a caller that queued some
    /// runs the driver before it waits.
    pub fn submitted(&self) -> u64 {
        self.submitted.get()
    }

    fn event(&self, event: Event<T>) {
        self.events.borrow_mut().push_back(event);
    }

    /// The disk is gone: what it holds is answered not done.
    fn end(&self, disk: u32) {
        let Some(d) = self.disks.borrow_mut().remove(&disk) else { return };
        for work in d.work {
            let write = matches!(work, Work::Io { write: true, .. });
            self.event(Event::Answered { disk, token: work.token(), write, ok: false, status: 0 });
        }
        self.event(Event::Gone { disk });
    }

    fn next_expiry(&self) -> Option<Nanos> {
        self.disks.borrow().values().filter_map(|d| d.away).min()
    }

    /// Ends every disk held past its device's return window.
    pub(crate) async fn keep<R: Registers, D: DmaBuffers, C: Host>(&self, usb: &Usb<R, D, C, T>) -> Result<(), Dead> {
        loop {
            let Some(at) = self.next_expiry() else {
                exec::until(&usb.wake, || self.next_expiry().map(|_| ())).await;
                continue;
            };
            let changed = exec::until_or(&usb.wake, at, || (self.next_expiry() != Some(at)).then_some(())).await;
            if changed.is_some() {
                continue;
            }
            let expired: Vec<u32> = self
                .disks
                .borrow()
                .iter()
                .filter(|(_, d)| d.away.is_some_and(|away| away <= at))
                .map(|(n, _)| *n)
                .collect();
            for disk in expired {
                usb.say(format_args!("disk {disk}: its device did not come back; it is gone"));
                self.end(disk);
            }
        }
    }
}

/// The disk a mass-storage interface serves, and its pipes.
struct Msc<'d, 'u, R: Registers, D: DmaBuffers, C: Host, T> {
    dev: &'d Device<'u, R, D, C, T>,
    interface: u8,
    bulk_in: Endpoint,
    bulk_out: Endpoint,
    in_packet: NonZeroU16,
    out_packet: NonZeroU16,
    /// CBW at 0, CSW at 64, a small data phase at 512.
    io: usize,
    /// A bring-up read's sectors.
    block: usize,
    tag: u32,
    run: Run,
    /// The ladder asked the port for something: this function ends.
    asked: bool,
}

const AT_CSW: usize = 64;
const AT_SMALL: usize = 512;

/// A data phase: where, which way, how long.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Data {
    None,
    In { at: u64, len: u32 },
    Out { at: u64, len: u32 },
}

impl Data {
    fn len(self) -> u32 {
        match self {
            Self::None => 0,
            Self::In { len, .. } | Self::Out { len, .. } => len,
        }
    }
}

pub async fn serve<R: Registers, D: DmaBuffers, C: Host, T>(
    dev: &Device<'_, R, D, C, T>,
    interface: Interface,
    binding: Binding<'_>,
) -> Result<(), Dead> {
    let usb = dev.usb;
    let name = format!("usb port {} interface {}", dev.port + 1, interface.number);
    let say = |line: core::fmt::Arguments<'_>| usb.say(format_args!("{name}: {line}"));
    let pipe = |inbound: bool| interface.endpoints.iter().copied().find(|e| e.kind == BULK && e.is_in() == inbound);
    let (Some(bulk_in), Some(bulk_out)) = (pipe(true), pipe(false)) else {
        say(format_args!("Bulk-Only with no bulk pair; not bound"));
        return Ok(());
    };
    let (Some(in_packet), Some(out_packet)) = (NonZeroU16::new(bulk_in.max_packet), NonZeroU16::new(bulk_out.max_packet)) else {
        say(format_args!("a bulk endpoint states a max packet size of 0; not bound"));
        return Ok(());
    };
    let (Some(io), Some(block)) = (dev.page(), dev.page()) else {
        say(format_args!("no pages left in the grant; not bound"));
        return Ok(());
    };
    let run = usb.runs.borrow_mut().remove(&dev.port).unwrap_or(Run::NONE);
    let mut msc = Msc { dev, interface: interface.number, bulk_in, bulk_out, in_packet, out_packet, io, block, tag: 0, run, asked: false };
    let serial = msc.serial().await?;
    let Some((geometry, inquiry)) = msc.bring_up(&say).await? else { return Ok(()) };
    let device = dev.descriptor.get().expect("enumerated");
    let identity = Identity {
        usb: UsbId { vendor: device.vendor, product: device.product, release: device.release },
        serial,
        inquiry: inquiry.0,
        sectors: geometry.sectors(),
        sector_bytes: geometry.sector_bytes(),
    };
    say(format_args!(
        "{} {} ({} sectors of {} bytes)",
        scsi::Printable(inquiry.vendor()),
        scsi::Printable(inquiry.product()),
        geometry.sectors(),
        geometry.sector_bytes()
    ));
    let disks = &usb.disks;
    let disk = match adopt(disks, &identity, usb) {
        Some(disk) => disk,
        None => {
            let table = msc.table(&geometry).await?;
            if msc.asked {
                return Ok(());
            }
            let disk = disks.next.get();
            disks.next.set(disk + 1);
            let held = Disk { identity, geometry, work: VecDeque::new(), away: None, reset_at: None, no_cache: false, debt: Debt::NONE };
            disks.disks.borrow_mut().insert(disk, held);
            disks.event(Event::Arrived { disk, name: format!("usb disk {disk}"), table });
            say(format_args!("serving it as disk {disk}"));
            disk
        }
    };
    drop(binding);
    msc.work(disk).await?;
    // The device left or was let go of: its disk is held for a device this
    // driver reset, and gone otherwise.
    let now = usb.wake.now();
    let mut held = disks.disks.borrow_mut();
    let d = held.get_mut(&disk).expect("served until here");
    match identity::returns_by(d.reset_at, now) {
        Some(by) if !msc.offline() => {
            d.away = Some(by);
            d.debt = Debt::adopted(d.debt.left(d.no_cache));
            drop(held);
            say(format_args!("disk {disk} is held for its device's return"));
        }
        _ => {
            drop(held);
            say(format_args!("disk {disk} is gone"));
            disks.end(disk);
        }
    }
    Ok(())
}

/// The held disk `identity` is the device of, taken back; its sessions are
/// told of a reset if it may have lost writes it reported.
fn adopt<R: Registers, D: DmaBuffers, C: Host, T>(disks: &Disks<T>, identity: &Identity, usb: &Usb<R, D, C, T>) -> Option<u32> {
    let mut all = disks.disks.borrow_mut();
    let (&disk, d) = all.iter_mut().find(|(_, d)| d.away.is_some() && identity::same(&d.identity, identity).is_ok())?;
    d.away = None;
    let lost = d.debt.losses() > 0;
    if lost {
        d.work.clear();
    }
    drop(all);
    usb.say(format_args!("disk {disk}'s device came back"));
    if lost {
        disks.event(Event::Reset { disk });
    }
    Some(disk)
}

impl<'d, 'u, R: Registers, D: DmaBuffers, C: Host, T> Msc<'d, 'u, R, D, C, T> {
    fn offline(&self) -> bool {
        self.dev.ask.get() == Some(Ask::Offline)
    }

    fn addr(&self, at: usize) -> u64 {
        self.dev.hc().dma.device_addr(at)
    }

    fn dci(&self, pipe: Pipe) -> u8 {
        match pipe {
            Pipe::In => self.bulk_in.dci(),
            Pipe::Out => self.bulk_out.dci(),
        }
    }

    /// The string its iSerialNumber names (USB 2.0 §9.6.7), in the first
    /// language string descriptor zero offers.
    async fn serial(&mut self) -> Result<Serial, Dead> {
        let index = self.dev.descriptor.get().map_or(0, |d| d.serial);
        if index == 0 {
            return Ok(Serial::Absent);
        }
        let Ok(languages) = self.dev.control(Setup::get_descriptor(3, 0, 255)).await? else { return Ok(Serial::Unread) };
        let Some(language) = identity::first_language(&languages) else { return Ok(Serial::Unread) };
        let ask = Setup { index: language, ..Setup::get_descriptor(3, index, 255) };
        Ok(match self.dev.control(ask).await? {
            Ok(bytes) => Serial::from_descriptor(&bytes),
            Err(_) => Serial::Unread,
        })
    }

    /// TEST UNIT READY on a budget, INQUIRY and READ CAPACITY
    /// (`toyos_scsi::BringUp`).
    async fn bring_up(&mut self, say: &dyn Fn(core::fmt::Arguments<'_>)) -> Result<Option<(Geometry, scsi::Inquiry)>, Dead> {
        let wake = self.dev.wake();
        let (mut up, mut ask) = scsi::BringUp::begin(wake.now() + READY_NS);
        let mut inquiry = None;
        let mut broke = None;
        loop {
            let small = self.addr(self.io + AT_SMALL);
            let heard_bytes: Vec<u8>;
            let heard = match ask {
                scsi::Ask::TestUnitReady => match self.bot(&Cdb::TEST_UNIT_READY, Data::None).await? {
                    Ok(Bot::Done { .. }) => scsi::Heard::Good,
                    Ok(Bot::Failed) => scsi::Heard::CheckCondition,
                    Err(b) => {
                        broke = Some(b);
                        scsi::Heard::Broke
                    }
                },
                scsi::Ask::RequestSense => scsi::Heard::Sense(self.sense().await?),
                scsi::Ask::Recover => {
                    let b = broke.take().expect("a break to recover from");
                    let back = self.climb(b, false).await?;
                    if self.asked {
                        return Ok(None);
                    }
                    scsi::Heard::Recovered { offline: !back }
                }
                scsi::Ask::Read(query) => {
                    let len = query.allocation() as u32;
                    match self.command(&query.cdb(), Data::In { at: small, len }).await? {
                        Reply::Ok { delivered } => {
                            heard_bytes = (0..len as usize).map(|i| self.dev.read8(self.io + AT_SMALL + i)).collect();
                            scsi::Heard::Data { bytes: &heard_bytes, delivered }
                        }
                        _ if self.asked => return Ok(None),
                        _ => scsi::Heard::Unanswered,
                    }
                }
            };
            match up.heard(heard, wake.now()) {
                scsi::Next::Ask(next, a) => (up, ask) = (next, a),
                scsi::Next::Disk(next, said, a) => {
                    inquiry = Some(said);
                    (up, ask) = (next, a);
                }
                scsi::Next::Up(scsi::Up::Ready(geometry)) => {
                    return Ok(Some((geometry, inquiry.expect("INQUIRY is read before the capacity"))));
                }
                scsi::Next::Up(scsi::Up::Unready { sense, offline }) => {
                    say(format_args!("it never said it was ready (last sense {sense}, offline: {offline}); not served"));
                    return Ok(None);
                }
                scsi::Next::Up(scsi::Up::Refused(why)) => {
                    say(format_args!("not served: {why:?}"));
                    return Ok(None);
                }
            }
        }
    }

    /// The table, as `Usb::table` reads it, its sectors fetched by replaying
    /// the read until it asks for none it has not been given.
    async fn table(&mut self, geometry: &Geometry) -> Result<T, Dead> {
        let per = HOST_BLOCK / geometry.sector_bytes();
        let mut cache = Cache { lba_bytes: geometry.sector_bytes(), lbas: geometry.sectors(), have: BTreeMap::new(), missed: Vec::new() };
        let mut reads = 0;
        loop {
            cache.missed.clear();
            let table = (self.dev.usb.table)(&mut cache);
            let missed = core::mem::take(&mut cache.missed);
            if missed.is_empty() || reads >= TABLE_READS {
                return Ok(table);
            }
            for lba in missed {
                let block = lba / u64::from(per);
                if cache.have.contains_key(&(block * u64::from(per))) {
                    continue;
                }
                reads += 1;
                let at = self.addr(self.block);
                // The last block of a disk whose count is not whole blocks is
                // read as far as the disk goes: the backup table is there.
                let start = block * u64::from(per);
                let sectors = u64::from(per).min(geometry.sectors() - start) as u32;
                let len = sectors * geometry.sector_bytes();
                let cdb = Cdb::transfer(false, start as u32, sectors as u16);
                let read = self.command(&cdb, Data::In { at, len }).await?;
                for s in 0..u64::from(sectors) {
                    let first = (s as usize) * geometry.sector_bytes() as usize;
                    let sector = match read {
                        Reply::Ok { delivered } if delivered == len => Some(
                            (0..geometry.sector_bytes() as usize).map(|i| self.dev.read8(self.block + first + i)).collect(),
                        ),
                        _ => None,
                    };
                    cache.have.insert(block * u64::from(per) + s, sector);
                }
                if self.asked {
                    return Ok(table);
                }
            }
        }
    }

    /// Serve disk `disk`'s queue until the device leaves or the ladder asks
    /// its port for something.
    async fn work(&mut self, disk: u32) -> Result<(), Dead> {
        let usb = self.dev.usb;
        let disks = &usb.disks;
        loop {
            let next = exec::until(self.dev.wake(), || {
                if self.dev.leaving.get() {
                    return Some(None);
                }
                disks.disks.borrow().get(&disk).and_then(|d| d.work.front().copied()).map(Some)
            })
            .await;
            let Some(work) = next else { return Ok(()) };
            let geometry = disks.disks.borrow()[&disk].geometry;
            let (ok, status, write) = match work {
                Work::Io { write, block, blocks, at, .. } => {
                    let (ok, status) = self.io(&geometry, write, block, blocks, at).await?;
                    (ok, status, write)
                }
                Work::Flush { .. } => {
                    let (ok, status, no_cache) = match scsi::flushed(self.command(&Cdb::SYNCHRONIZE_CACHE, Data::None).await?) {
                        scsi::Flushed::Emptied => (true, 0, false),
                        scsi::Flushed::NoCache => (true, 0, true),
                        scsi::Flushed::Refused(sense) => (false, status_of(sense), false),
                        scsi::Flushed::Ended(_) => (false, 0, false),
                    };
                    if let Some(d) = disks.disks.borrow_mut().get_mut(&disk) {
                        if ok {
                            d.debt.flushed();
                        }
                        d.no_cache |= no_cache;
                    }
                    (ok, status, false)
                }
            };
            if self.asked || self.dev.leaving.get() {
                // Unanswered: it is issued again to the device that comes back,
                // or answered with the disk if none does.
                if self.dev.ask.get() == Some(Ask::Reenumerate)
                    && let Some(d) = disks.disks.borrow_mut().get_mut(&disk)
                {
                    d.reset_at = Some(usb.wake.now());
                }
                return Ok(());
            }
            let mut all = disks.disks.borrow_mut();
            let d = all.get_mut(&disk).expect("served");
            d.work.pop_front();
            if write && ok {
                d.debt.wrote();
            }
            drop(all);
            disks.event(Event::Answered { disk, token: work.token(), write, ok, status });
        }
    }

    /// One read or write, in as many READ(10)s or WRITE(10)s as it takes.
    async fn io(&mut self, geometry: &Geometry, write: bool, block: u64, blocks: u32, at: u64) -> Result<(bool, u32), Dead> {
        let mut transfer = match scsi::Transfer::new(block, blocks, write, geometry, toyos_blockring::MAX_REQUEST_BLOCKS) {
            Ok(t) => t,
            Err(past) => {
                self.dev.usb.say(format_args!("a transfer past the disk's end refused: {past:?}"));
                return Ok((false, 0));
            }
        };
        while let Some(batch) = transfer.next() {
            let at = at + batch.offset as u64;
            let len = batch.bytes as u32;
            let data = if write { Data::Out { at, len } } else { Data::In { at, len } };
            let reply = self.command(&batch.cdb, data).await?;
            match transfer.answered(&batch, reply) {
                scsi::Moved::Whole => {}
                scsi::Moved::Refused(sense) => return Ok((false, status_of(sense))),
                _ => return Ok((false, 0)),
            }
        }
        Ok((true, 0))
    }

    fn next_tag(&mut self) -> u32 {
        self.tag = self.tag.wrapping_add(1);
        self.tag
    }

    /// A command with the ladder behind it.
    async fn command(&mut self, cdb: &Cdb, data: Data) -> Result<Reply, Dead> {
        loop {
            if self.asked || self.dev.leaving.get() {
                return Ok(Reply::Broken);
            }
            match self.bot(cdb, data).await? {
                Ok(Bot::Done { delivered }) => {
                    self.run.over();
                    return Ok(Reply::Ok { delivered });
                }
                Ok(Bot::Failed) => {
                    self.run.over();
                    return Ok(Reply::Refused(self.sense().await?));
                }
                Err(Broke::Gone { .. }) => return Ok(Reply::Broken),
                Err(broke) => {
                    if !self.climb(broke, matches!(data, Data::Out { .. })).await? {
                        return Ok(Reply::Broken);
                    }
                }
            }
        }
    }

    /// REQUEST SENSE, read and judged; [`Sense::NONE`] where it says nothing
    /// this driver reads.
    async fn sense(&mut self) -> Result<Sense, Dead> {
        let at = self.addr(self.io + AT_SMALL);
        let delivered = match self.bot(&Cdb::REQUEST_SENSE, Data::In { at, len: SENSE_BYTES as u32 }).await? {
            Ok(Bot::Done { delivered }) => delivered,
            _ => return Ok(Sense::NONE),
        };
        let mut bytes = [0u8; SENSE_BYTES];
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = self.dev.read8(self.io + AT_SMALL + i);
        }
        Ok(Sense::of(&bytes, delivered).unwrap_or_else(|why| {
            self.dev.usb.say(format_args!("its sense is refused: {why:?}"));
            Sense::NONE
        }))
    }

    /// The ladder's next rung for `broke`: `true` once the transport is
    /// verified back, `false` once the device is asked away.
    async fn climb(&mut self, broke: Broke<Silent>, data_out: bool) -> Result<bool, Dead> {
        let mut left = broke.left(data_out);
        self.dev.usb.say(format_args!("a Bulk-Only round trip broke: {broke:?}"));
        loop {
            let climb = self.run.broke(left);
            match climb.rung {
                Rung::ClassReset => {
                    if self.class_reset().await? {
                        let tag_check = self.bot(&Cdb::TEST_UNIT_READY, Data::None).await?;
                        if tag_check.is_ok() {
                            self.dev.usb.say(format_args!("Reset Recovery brought the transport back"));
                            return Ok(true);
                        }
                    }
                    left = Left::Elsewhere;
                }
                rung @ (Rung::PortReset | Rung::Offline) => {
                    self.dev.usb.say(format_args!("the ladder climbs to its {}", rung.named()));
                    self.asked = true;
                    let ask = if rung == Rung::PortReset { Ask::Reenumerate } else { Ask::Offline };
                    if ask == Ask::Reenumerate {
                        self.dev.usb.runs.borrow_mut().insert(self.dev.port, self.run);
                    }
                    self.dev.ask.set(Some(ask));
                    return Ok(false);
                }
            }
        }
    }

    /// Bulk-Only Reset Recovery (§5.3.4): both pipes off their transfers, the
    /// class reset and a CLEAR_FEATURE(ENDPOINT_HALT) to each, which zero the
    /// device's toggles whether or not either was halted (USB 2.0 §9.4.5),
    /// and both endpoints re-created with the controller's zeroed (xHCI 1.2
    /// §4.6.8's note).
    async fn class_reset(&mut self) -> Result<bool, Dead> {
        let dev = self.dev;
        for ep in [self.bulk_in, self.bulk_out] {
            let id = u32::from(dev.slot) << 24 | u32::from(ep.dci()) << 16;
            let command = match dev.endpoint_state(ep.dci()) {
                crate::recovery::EndpointState::Running => hc::TRB_STOP_ENDPOINT,
                crate::recovery::EndpointState::Halted => hc::TRB_RESET_ENDPOINT,
                _ => continue,
            };
            let done = dev.hc().command(dev.wake(), Trb { control: command | id, ..Trb::default() }).await?;
            if !done.succeeded() {
                return Ok(false);
            }
        }
        let index = u16::from(self.interface);
        let reset = Setup { request_type: 0x21, request: 0xFF, value: 0, index, length: 0 };
        if dev.control(reset).await?.is_err() {
            return Ok(false);
        }
        for ep in [self.bulk_in, self.bulk_out] {
            let clear = Setup { request_type: 0x02, request: 1, value: 0, index: u16::from(ep.address), length: 0 };
            if dev.control(clear).await?.is_err() {
                return Ok(false);
            }
        }
        Ok(dev.configure(&[self.bulk_in, self.bulk_out], Configure::Again).await?.is_ok())
    }

    /// One round trip, as `bot::RoundTrip` orders it.
    async fn bot(&mut self, cdb: &Cdb, data: Data) -> Result<Result<Bot, Broke<Silent>>, Dead> {
        let tag = self.next_tag();
        let (mut trip, mut act) = RoundTrip::begin(tag, data.len(), cdb);
        loop {
            let answer = match act {
                bot::Act::Command => {
                    self.dev.write_bytes(self.io, &bot::cbw(tag, data.len(), cdb));
                    self.transfer(Pipe::Out, &[(self.addr(self.io), CBW_LEN as u32)]).await?
                }
                bot::Act::Data(pipe) => {
                    let (at, len) = match data {
                        Data::In { at, len } | Data::Out { at, len } => (at, len),
                        Data::None => unreachable!("no data phase is asked of a command without one"),
                    };
                    self.transfer(pipe, &pieces(at, len)).await?
                }
                bot::Act::Status => {
                    self.dev.write_bytes(self.io + AT_CSW, &[0; 16]);
                    self.transfer(Pipe::In, &[(self.addr(self.io + AT_CSW), CSW_LEN as u32)]).await?
                }
                bot::Act::Restart { pipe, .. } => Answer::Restarted(self.dev.restart(self.dci(pipe), None).await?.is_ok()),
            };
            match trip.answered(answer) {
                bot::Next::Act(next, a) => (trip, act) = (next, a),
                bot::Next::Csw(due) => {
                    let mut csw = [0u8; CSW_LEN];
                    for (i, b) in csw.iter_mut().enumerate() {
                        *b = self.dev.read8(self.io + AT_CSW + i);
                    }
                    return Ok(due.judge(&csw));
                }
                bot::Next::Broke(broke) => return Ok(Err(broke)),
            }
        }
    }

    /// One bulk transfer of `pieces`, answered, silent or gone; a silent one
    /// is taken back from the controller before it is answered.
    async fn transfer(&mut self, pipe: Pipe, pieces: &[(u64, u32)]) -> Result<Answer<Silent>, Dead> {
        if self.dev.leaving.get() {
            return Ok(Answer::Gone);
        }
        let dci = self.dci(pipe);
        let total: u32 = pieces.iter().map(|(_, len)| len).sum();
        let max_packet = match pipe {
            Pipe::In => self.in_packet,
            Pipe::Out => self.out_packet,
        };
        let trbs = hc::normal_td(pieces, max_packet);
        let mut op = self.dev.submit(dci, &trbs);
        let deadline = self.dev.wake().now() + COMMAND_NS;
        Ok(match op.answer(self.dev.wake(), deadline).await {
            Some(Done::Gone) => Answer::Gone,
            Some(Done::Event { code, moved, .. }) => Answer::Moved { code, residue: total.saturating_sub(moved) },
            None => {
                let _ = self.dev.restart(dci, Some(op)).await?;
                Answer::Silent(Silent)
            }
        })
    }
}

/// `len` bytes from `at` as TRB-sized pieces, none crossing a 64 KiB
/// boundary (xHCI 1.2 §6.4.1).
fn pieces(at: u64, len: u32) -> Vec<(u64, u32)> {
    let mut out = Vec::new();
    let (mut at, mut left) = (at, u64::from(len));
    while left > 0 {
        let room = TRB_MOST - (at % TRB_MOST);
        let take = room.min(left);
        out.push((at, take as u32));
        at += take;
        left -= take;
    }
    out
}

fn status_of(sense: Sense) -> u32 {
    u32::from(sense.key) << 16 | u32::from(sense.asc) << 8 | u32::from(sense.ascq)
}

/// The disk's sectors as a table read asks for them: those fetched, and
/// every one asked for that was not.
struct Cache {
    lba_bytes: u32,
    lbas: u64,
    /// `None` for a sector whose read failed.
    have: BTreeMap<u64, Option<Vec<u8>>>,
    missed: Vec<u64>,
}

impl toyos_gpt::Sectors for Cache {
    fn lba_bytes(&self) -> u32 {
        self.lba_bytes
    }
    fn lba_count(&self) -> u64 {
        self.lbas
    }
    fn read_lba(&mut self, lba: u64, buf: &mut [u8]) -> bool {
        match self.have.get(&lba) {
            Some(Some(sector)) => {
                buf.copy_from_slice(sector);
                true
            }
            Some(None) => false,
            None => {
                self.missed.push(lba);
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_piece_crosses_a_64_kib_boundary() {
        assert_eq!(pieces(0x1_0000, 0x2_0000), [(0x1_0000, 0x1_0000), (0x2_0000, 0x1_0000)]);
        assert_eq!(pieces(0xF000, 0x3000), [(0xF000, 0x1000), (0x1_0000, 0x2000)]);
        for at in [0u64, 0x200, 0xFFFF, 0x1_2345] {
            for len in [1u32, 0x1000, 0x2_0000] {
                let p = pieces(at, len);
                assert_eq!(p.iter().map(|(_, l)| l).sum::<u32>(), len);
                assert!(p.iter().all(|&(a, l)| a / TRB_MOST == (a + u64::from(l) - 1) / TRB_MOST));
            }
        }
    }
}
