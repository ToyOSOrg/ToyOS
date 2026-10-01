//! blockd: one NVMe controller, driven from userland, serving its partitions
//! as `toyos-blockring` sessions.
//!
//! **It holds a claim on one PCI function and nothing else of the machine.**
//! The claim arrives under the `dev:pci:` label its starter minted it under —
//! init for a manifest row, or a test's supervisor — and the port it serves
//! under `serve:block`; the kernel keeps config space, the interrupt vector
//! and the function's address space, and blockd never names an address the
//! kernel did not hand it ([`nvme`]).
//!
//! **A session is a partition held**, from the client's open to its hang-up:
//! one holder per partition (`toyos-blockhold`), every request bounded to the
//! partition and the arena before the device sees it
//! (`toyos_blockring::server`), the data moved by the device straight into and
//! out of the client's own region, which the kernel maps into this
//! controller's domain for the length of the session (`SYS_DEVICE_DMA_MAP`)
//! and takes back at its end — after the last command naming it has been
//! answered, never before.
//!
//! **The partition table is read once, at start.** Every partition lies
//! outside every other and the table outside all of them, so no session can
//! change it; the table this process read is the one it serves for its life.
//! A partition whose range is not whole 4 KiB blocks, or whose GUID the table
//! carries twice, is listed and refused by name.
//!
//! **A controller its row names that is on the machine and whose claim the
//! kernel refused is not an absent one**: its starter names it
//! (`--claim-refused <name>`), and a listing and every open are refused
//! `ClaimRefused`, so a file server says so rather than finding no disk.
//!
//! **The partition the machine runs from is refused to every session**: its
//! starter names it (`--running <guid>`, the ROOT the loader read), because a
//! writer there changes the image under the kernel that booted from it.
//!
//! **A client may ask what is served** ([`wire::MSG_LIST`]) before it opens
//! anything, which is how a file server finds its role's partition by type.
//!
//! **A server never blocks on a client.** Accept and the open frame are two
//! events, the open is buffered until whole, every answer is one `try_send`,
//! and a doorbell is one non-blocking write whose refusal is a full pipe —
//! a doorbell already rung.



use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use blockd::region::Region;
use blockd::nvme::{Controller, Done, Owner};
use toyos::endow::{self, Endowments};
use toyos::ipc::{self, Connection, RxStep};
use toyos::poller::{Poller, READABLE};
use toyos::AsHandle;
use toyos_abi::part::{PartGuid, GUID_TEXT_LEN};
use toyos_abi::syscall::{DEV_PREFIX, SyscallError};
use toyos_blockhold::Holds;
use toyos_blockring::entry::{Completion, Op};
use toyos_blockring::layout::{self, ServerRings, DEPTH};
use toyos_blockring::server::{ServerSession, Taken};
use toyos_blockring::wire::{self, Opened, Refusal};
use toyos_blockring::{BLOCK_BYTES, PORT, SESSION_BYTES};

/// How long a command may go unanswered before the controller is reset to
/// take it back. Policy: NVMe defines no per-command bound, and a flush of a
/// large cache is the slowest thing a healthy drive does.
const COMMAND_SILENCE: Duration = Duration::from_secs(10);

/// Sessions held at once. Each costs its region's mapping in this process and
/// in the controller's domain, which the kernel bounds per claim.
const MAX_SESSIONS: usize = 8;

/// Connections accepted and not yet opened, and how long one may stay so.
const MAX_PENDING: usize = 16;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);

/// Argv, followed by a unique GUID: the partition the machine runs from, which
/// no session opens.
const RUNNING: &str = "--running";

/// Argv, followed by a device name of this service's row: on this machine, and
/// its claim refused.
const CLAIM_REFUSED: &str = "--claim-refused";

const TOKEN_IRQ: u64 = 0;
const TOKEN_ACCEPT: u64 = 1;
const TOKEN_PENDING: u64 = 0x1_0000;
const TOKEN_SESSION: u64 = 0x2_0000;

/// One partition of the table, in 4 KiB blocks of the device.
struct Part {
    unique: [u8; 16],
    kind: [u8; 16],
    /// Where it starts and how long it is, or why it cannot be served.
    span: Result<(u64, u64), String>,
}

fn guid_text(bytes: [u8; 16]) -> String {
    let mut buf = [0u8; GUID_TEXT_LEN];
    PartGuid(bytes).write_text(&mut buf).to_string()
}

/// The controller as `toyos_gpt` reads it: its own sectors, served a 4 KiB
/// block at a time, and floored to whole blocks.
struct Sectors<'a> {
    ctrl: &'a mut Controller,
    block: Vec<u8>,
}

impl toyos_gpt::Sectors for Sectors<'_> {
    fn lba_bytes(&self) -> u32 {
        self.ctrl.lba_bytes
    }
    fn lba_count(&self) -> u64 {
        let per = BLOCK_BYTES as u64 / self.ctrl.lba_bytes as u64;
        self.ctrl.sectors / per * per
    }
    fn lba_count_granularity(&self) -> core::num::NonZeroU64 {
        core::num::NonZeroU64::new(BLOCK_BYTES as u64 / self.ctrl.lba_bytes as u64).expect("at least one")
    }
    fn read_lba(&mut self, lba: u64, buf: &mut [u8]) -> bool {
        let lba_bytes = self.ctrl.lba_bytes as u64;
        let at = lba * lba_bytes;
        if !self.ctrl.read_block(at / BLOCK_BYTES as u64, &mut self.block) {
            return false;
        }
        let off = (at % BLOCK_BYTES as u64) as usize;
        buf.copy_from_slice(&self.block[off..off + lba_bytes as usize]);
        true
    }
}

/// Every partition the table names, each checked the way the kernel checks a
/// claim's: past `toyos_gpt::locate`'s range and overlap checks, and whole
/// 4 KiB blocks.
fn read_table(ctrl: &mut Controller) -> Vec<Part> {
    let per = BLOCK_BYTES as u64 / ctrl.lba_bytes as u64;
    let mut sectors = Sectors { ctrl, block: vec![0u8; BLOCK_BYTES] };
    let mut found = [None; 128];
    if let Err(why) = toyos_gpt::list(&mut sectors, &mut found) {
        println!("blockd: the disk carries no partition table this driver reads: {why:?}");
        return Vec::new();
    }
    let mut parts = Vec::new();
    for entry in found.iter().flatten() {
        let listed = match entry {
            Ok(listed) => listed,
            Err(unplaced) => {
                let span = Err(format!("its blocks are no partition on this disk: {unplaced:?}"));
                parts.push(Part { unique: unplaced.unique_guid.0, kind: unplaced.type_guid.0, span });
                continue;
            }
        };
        let unique = listed.unique_guid().0;
        let kind = listed.type_guid().0;
        let span = match toyos_gpt::locate(&mut sectors, listed.unique_guid()) {
            Err(why) => Err(format!("its own table refuses it: {why:?}")),
            Ok(located) => {
                let p = located.partition();
                let (first, count) = (p.first_lba(), p.lba_count().get());
                if first % per != 0 || count % per != 0 {
                    Err(format!(
                        "LBA {first}+{count} is not whole 4 KiB blocks of {}-byte sectors",
                        sectors.ctrl.lba_bytes
                    ))
                } else {
                    Ok((first / per, count / per))
                }
            }
        };
        parts.push(Part { unique, kind, span });
    }
    parts
}

/// A connection that has not opened a session yet.
struct Pending {
    conn: Connection,
    rx: ipc::FrameRx<{ wire::GUID_BYTES }>,
    since: Instant,
}

/// A partition held, over a client's region.
struct Served {
    conn: Connection,
    region: Region,
    device_addr: u64,
    rings: ServerRings,
    state: ServerSession,
    unique: [u8; 16],
    /// The client hung up, or broke the protocol: nothing more is taken, and
    /// the session ends once nothing of it is on the device.
    closing: bool,
    requests: u64,
    posted: bool,
}

/// The controller this service drives and the partitions its table names, or
/// why it drives none. Without one no partition is listed and no session
/// opened, and a connection says one frame and is answered, or is let go.
enum Drive {
    Up(Controller, Vec<Part>),
    /// No controller this row names is on this machine: a listing is empty
    /// and an open `NotFound`.
    Absent,
    /// A controller this service cannot use: a listing and an open are
    /// refused `Unusable`, so a file server says the disk failed rather than
    /// that there is none.
    Unusable,
    /// A controller this row names is on this machine and its claim was
    /// refused: a listing and an open are refused `ClaimRefused`.
    ClaimRefused,
}

impl Drive {
    /// The controller a session is on: a service without one has no partition
    /// to open.
    fn up(&mut self) -> &mut Controller {
        match self {
            Drive::Up(ctrl, _) => ctrl,
            Drive::Absent | Drive::Unusable | Drive::ClaimRefused => {
                unreachable!("blockd: a session with no controller")
            }
        }
    }

    fn oldest(&self) -> Option<Instant> {
        match self {
            Drive::Up(ctrl, _) => ctrl.oldest(),
            Drive::Absent | Drive::Unusable | Drive::ClaimRefused => None,
        }
    }
}

struct Service {
    ctrl: Drive,
    holds: Holds<u64>,
    /// The device's loss count: every reset may have dropped its cache.
    losses: u64,
    sessions: BTreeMap<u64, Served>,
    next_id: u64,
    /// The partition the machine is running from, as the loader named it:
    /// refused to every session, since a writer there changes the image
    /// under the kernel that booted from it.
    running: Option<[u8; 16]>,
}

/// A session decided on and not yet told to its client.
struct Opening {
    region: Region,
    device_addr: u64,
    first: u64,
    blocks: u64,
    unique: [u8; 16],
}

impl Service {
    fn new(ctrl: Drive, running: Option<[u8; 16]>) -> Self {
        Self { ctrl, holds: Holds::new(), losses: 0, sessions: BTreeMap::new(), next_id: 0, running }
    }

    /// What a listing answers: every partition the table names.
    fn listing(&self) -> Result<Vec<u8>, Refusal> {
        match &self.ctrl {
            Drive::Up(_, parts) => {
                Ok(parts.iter().flat_map(|p| wire::Listed { unique: p.unique, kind: p.kind }.encode()).collect())
            }
            Drive::Absent => Ok(Vec::new()),
            Drive::Unusable => Err(Refusal::Unusable),
            Drive::ClaimRefused => Err(Refusal::ClaimRefused),
        }
    }

    /// The span an open of `guid` is served on, held for it; or its refusal.
    fn place(&mut self, guid: [u8; 16]) -> Result<(u64, u64), Refusal> {
        let parts = match &self.ctrl {
            Drive::Up(_, parts) => parts,
            Drive::Absent => return Err(Refusal::NotFound),
            Drive::Unusable => return Err(Refusal::Unusable),
            Drive::ClaimRefused => return Err(Refusal::ClaimRefused),
        };
        let part = parts.iter().find(|p| p.unique == guid && guid != [0; 16]);
        let (first, blocks) = match part.map(|p| &p.span) {
            None => return Err(Refusal::NotFound),
            Some(Err(_)) => return Err(Refusal::Unusable),
            Some(Ok(span)) => *span,
        };
        if self.running == Some(guid) {
            return Err(Refusal::Held);
        }
        if self.sessions.len() >= MAX_SESSIONS {
            return Err(Refusal::Exhausted);
        }
        if self.holds.hold(first, first + blocks, self.next_id).is_err() {
            return Err(Refusal::Held);
        }
        Ok((first, blocks))
    }

    /// What an open answers: a session to admit, or its refusal. `region` is
    /// the client's and is consumed either way.
    fn open(&mut self, guid: [u8; 16], region: toyos::RawHandle) -> Result<Opening, Refusal> {
        let region = Region::adopt(region).map_err(|_| Refusal::Malformed)?;
        let (first, blocks) = self.place(guid)?;
        match self.ctrl.up().claim().dma_map(region.handle()) {
            Ok(mapping) if mapping.bytes == SESSION_BYTES as u64 => {
                Ok(Opening { region, device_addr: mapping.device_addr, first, blocks, unique: guid })
            }
            // A region longer than a session would spend the claim's bound on
            // the kernel's side for every other client: refused whole.
            Ok(mapping) => {
                if let Err(why) = self.ctrl.up().claim().dma_unmap(mapping.device_addr) {
                    panic!("blockd: the kernel would not take an oversized region back: {why:?}");
                }
                self.holds.release(first);
                Err(Refusal::Malformed)
            }
            Err(why) => {
                self.holds.release(first);
                Err(match why {
                    SyscallError::ResourceExhausted => Refusal::Exhausted,
                    _ => Refusal::Malformed,
                })
            }
        }
    }

    /// The client heard its session is open: from here it is served.
    fn admit(&mut self, opening: Opening, conn: Connection) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let rings = layout::server(opening.region.words());
        self.sessions.insert(
            id,
            Served {
                conn,
                region: opening.region,
                device_addr: opening.device_addr,
                rings,
                state: ServerSession::new(opening.first, opening.blocks),
                unique: opening.unique,
                closing: false,
                requests: 0,
                posted: false,
            },
        );
        id
    }

    /// The client went before it heard: nothing of the session reached the
    /// device, and what was taken for it goes back.
    fn abandon(&mut self, opening: Opening) {
        if let Err(why) = self.ctrl.up().claim().dma_unmap(opening.device_addr) {
            panic!("blockd: the kernel would not take an abandoned session's region back: {why:?}");
        }
        self.holds.release(opening.first);
    }

    /// Put `c` on its session's completion ring. [`Self::pull`] leaves room for
    /// every answer, so a ring without it is a client whose head went back or
    /// past what was posted: the session ends.
    fn post(session: &mut Served, c: Completion) {
        match session.rings.1.push(session.region.words(), c.encode()) {
            Ok(true) => session.posted = true,
            Ok(false) | Err(_) => session.closing = true,
        }
    }

    /// Hand one device answer to the session it is for.
    fn deliver(&mut self, done: Done) {
        let Owner::Session { session, tag, .. } = done.owner else {
            panic!("blockd: the device answered a command of this driver's own while serving");
        };
        let s = self.sessions.get_mut(&session).expect("blockd: an answer for a session that ended");
        if let Some(c) = s.state.complete(tag, done.ok, &mut self.holds, self.losses) {
            if !done.ok {
                println!("blockd: session {session}: request {tag} failed with status {:#x}", done.status);
            }
            if c.status == toyos_blockring::Status::Lost {
                println!(
                    "blockd: session {session}: a flush found writes of its own the device lost \
                     since it acknowledged them; the client is told Lost"
                );
            }
            Self::post(s, c);
        }
    }

    /// Take what `id`'s client has published, while the device and the
    /// session's completion ring have room for it.
    fn pull(&mut self, id: u64) {
        let s = self.sessions.get_mut(&id).expect("a live session");
        loop {
            if s.closing {
                break;
            }
            let page = s.region.words();
            // Room for every answer: what is on the device, and what is posted
            // and not yet read, never exceeds the completion ring.
            let Ok(space) = s.rings.1.space(page) else {
                s.closing = true;
                break;
            };
            let unread = (DEPTH - space) as usize;
            if s.state.inflight().len() + unread >= DEPTH as usize || !self.ctrl.up().has_room() {
                break;
            }
            let words = match s.rings.0.pop(page) {
                Ok(Some(words)) => words,
                Ok(None) => break,
                Err(_) => {
                    s.closing = true;
                    break;
                }
            };
            s.requests += 1;
            match s.state.take(words) {
                Taken::Answer(c) => Self::post(s, c),
                Taken::Issue(req) => {
                    let write = matches!(req.op, Op::Write { .. });
                    let owner = Owner::Session { session: id, tag: req.tag, write };
                    match req.op {
                        Op::Read { run, lba } | Op::Write { run, lba } => {
                            let at = s.device_addr + run.span().offset as u64;
                            let block = s.state.first() + lba;
                            self.ctrl.up().submit_io(write, block, run.count(), at, owner);
                        }
                        Op::Flush if self.ctrl.up().vwc => self.ctrl.up().submit_flush(owner),
                        // No volatile cache: every write answered is on the
                        // medium already.
                        Op::Flush => {
                            let c = s.state.complete(req.tag, true, &mut self.holds, self.losses);
                            Self::post(s, c.expect("just taken"));
                        }
                    }
                }
            }
        }
        s.rings.0.release(s.region.words());
    }

    /// Publish what each session was answered, and ring its doorbell.
    fn publish(&mut self) {
        for s in self.sessions.values_mut() {
            if !s.posted {
                continue;
            }
            s.posted = false;
            if s.rings.1.publish(s.region.words()) {
                match s.conn.write_nonblock(&[1]) {
                    Ok(_) | Err(SyscallError::WouldBlock) => {}
                    Err(_) => s.closing = true,
                }
            }
        }
    }

    /// End every session that is closing and has nothing on the device: its
    /// region leaves the controller's domain, and its partition is free.
    fn retire(&mut self) {
        let done: Vec<u64> = self
            .sessions
            .iter()
            .filter(|(_, s)| s.closing && s.state.inflight().len() == 0)
            .map(|(id, _)| *id)
            .collect();
        for id in done {
            let s = self.sessions.remove(&id).expect("just listed");
            let ctrl = self.ctrl.up();
            if let Err(why) = ctrl.claim().dma_unmap(s.device_addr) {
                panic!("blockd: the kernel would not take session {id}'s region back: {why:?}");
            }
            self.holds.release(s.state.first());
            println!(
                "blockd: session {id} on {} closed after {} requests; the device has held at most {} \
                 commands at once, issued per queue {}",
                guid_text(s.unique),
                s.requests,
                ctrl.peak,
                ctrl.spread()
            );
        }
    }

    /// Reset the controller under every command it holds.
    fn reset(&mut self) {
        println!(
            "blockd: a command went unanswered for {COMMAND_SILENCE:?}; resetting the controller"
        );
        let mut done = Vec::new();
        let aborted = self
            .ctrl
            .up()
            .reset(&mut done)
            .unwrap_or_else(|why| panic!("blockd: the controller did not come back from its reset: {why}"));
        for d in done {
            self.deliver(d);
        }
        self.losses += 1;
        let mut answered = 0;
        for s in self.sessions.values_mut() {
            for c in s.state.abort_all() {
                answered += 1;
                Self::post(s, c);
            }
        }
        println!(
            "blockd: controller reset; {} commands it held were answered not done ({answered} of \
             them sessions'), and the loss count is {}",
            aborted.len(),
            self.losses
        );
    }
}

/// The PCI function this process was endowed.
fn claim() -> Option<toyos::PciDev> {
    let label = Endowments::get()
        .labels()
        .find(|l| l.starts_with(DEV_PREFIX) && l[DEV_PREFIX.len()..].starts_with("pci:"))
        .map(str::to_string)?;
    Some(Endowments::get().take(&label).expect("blockd: the claim its label names"))
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let running = args.iter().position(|a| a == RUNNING).map(|at| {
        args.get(at + 1)
            .and_then(|text| PartGuid::parse(text))
            .map(|guid| guid.0)
            .unwrap_or_else(|| panic!("blockd: {RUNNING} takes the running ROOT's unique GUID"))
    });
    let refused = args.iter().position(|a| a == CLAIM_REFUSED).map(|at| {
        args.get(at + 1).unwrap_or_else(|| panic!("blockd: {CLAIM_REFUSED} takes the device its claim was refused on"))
    });
    let acceptor = endow::acceptor(PORT).unwrap_or_else(|| panic!("blockd: started serving no `{PORT}` port"));
    if let Some(name) = refused {
        println!(
            "blockd: NOT SERVING — {name} is on this machine and the kernel refused this service its \
             claim; every partition on it is refused"
        );
        serve(&mut Service::new(Drive::ClaimRefused, running), &acceptor);
    }
    let Some(dev) = claim() else {
        println!("blockd: no NVMe controller this row names is on this machine; serving no partition");
        serve(&mut Service::new(Drive::Absent, running), &acceptor);
    };
    let mut ctrl = match Controller::open(dev) {
        Ok(ctrl) => ctrl,
        Err(why) => {
            println!("blockd: NOT SERVING — {why}; serving no partition");
            serve(&mut Service::new(Drive::Unusable, running), &acceptor);
        }
    };
    println!(
        "blockd: NVMe up: {} I/O queues of {} commands, volatile write cache {}, {}-byte sectors, \
         {} sectors",
        ctrl.queues(),
        blockd::nvme::COMMANDS_PER_QUEUE,
        if ctrl.vwc { "present, so a flush issues Flush" } else { "absent, so a flush issues nothing" },
        ctrl.lba_bytes,
        ctrl.sectors
    );
    let parts = read_table(&mut ctrl);
    for part in &parts {
        match &part.span {
            Ok(_) if running == Some(part.unique) => println!(
                "blockd: partition {} is the ROOT this machine runs from; refusing every session to it",
                guid_text(part.unique)
            ),
            Ok((first, blocks)) => {
                println!("blockd: partition {} at block {first}, {blocks} blocks", guid_text(part.unique))
            }
            Err(why) => println!("blockd: partition {} is not served: {why}", guid_text(part.unique)),
        }
    }
    serve(&mut Service::new(Drive::Up(ctrl, parts), running), &acceptor);
}

fn serve(service: &mut Service, acceptor: &toyos::port::Acceptor) -> ! {
    let poller = Poller::new(2 + MAX_PENDING as u32 + MAX_SESSIONS as u32);
    let mut pending: Vec<Pending> = Vec::new();
    let mut ready: Vec<u64> = Vec::new();
    let mut done: Vec<Done> = Vec::new();
    loop {
        if let Drive::Up(ctrl, _) = &service.ctrl {
            poller.watch(ctrl.claim(), READABLE, TOKEN_IRQ);
        }
        if pending.len() < MAX_PENDING {
            poller.watch(acceptor, READABLE, TOKEN_ACCEPT);
        }
        for p in &pending {
            poller.watch(&p.conn, READABLE, TOKEN_PENDING + p.conn.as_handle().0 as u64);
        }
        for (id, s) in &service.sessions {
            if !s.closing {
                poller.watch(&s.conn, READABLE, TOKEN_SESSION + id);
            }
        }
        let now = Instant::now();
        let timeout = pending
            .iter()
            .map(|p| HANDSHAKE_TIMEOUT.saturating_sub(now.duration_since(p.since)))
            .chain(service.ctrl.oldest().map(|at| COMMAND_SILENCE.saturating_sub(now.duration_since(at))))
            .min()
            .map_or(u64::MAX, |left| left.as_nanos() as u64);
        ready.clear();
        poller.wait(1, timeout, |token| ready.push(token));

        if let Drive::Up(ctrl, _) = &mut service.ctrl {
            if let Err(why) = ctrl.take_interrupt() {
                panic!(
                    "blockd: the claim refused its interrupt read ({why:?}): the unit refused this \
                     controller an access, and the claim answers nothing now"
                );
            }
            ctrl.reap(&mut done);
        }
        for d in done.drain(..) {
            service.deliver(d);
        }
        if service.ctrl.oldest().is_some_and(|at| at.elapsed() >= COMMAND_SILENCE) {
            service.reset();
        }

        let now = Instant::now();
        pending.retain(|p| now.duration_since(p.since) < HANDSHAKE_TIMEOUT);
        if ready.contains(&TOKEN_ACCEPT) {
            match acceptor.accept() {
                Ok(conn) => pending.push(Pending { conn, rx: ipc::FrameRx::new(), since: now }),
                Err(why) => panic!("blockd: its own acceptor refused an accept: {why:?}"),
            }
        }
        let mut i = 0;
        while i < pending.len() {
            let token = TOKEN_PENDING + pending[i].conn.as_handle().0 as u64;
            if !ready.contains(&token) {
                i += 1;
                continue;
            }
            let step = {
                let p = &mut pending[i];
                p.rx.pump(&p.conn)
            };
            match step {
                RxStep::Idle => i += 1,
                RxStep::Eof | RxStep::Malformed => {
                    pending.remove(i);
                }
                RxStep::Frame { msg_type, payload_len } => {
                    let p = pending.remove(i);
                    handshake(service, p, msg_type, payload_len);
                }
            }
        }

        let ids: Vec<u64> = service.sessions.keys().copied().collect();
        for id in ids {
            if ready.contains(&(TOKEN_SESSION + id)) {
                let s = service.sessions.get_mut(&id).expect("listed");
                if !doorbells(&s.conn) {
                    s.closing = true;
                }
            }
            service.pull(id);
        }
        service.publish();
        service.retire();
    }
}

/// Consume a session's doorbell bytes; `false` once its client has hung up.
fn doorbells(conn: &Connection) -> bool {
    let mut sink = [0u8; 64];
    loop {
        match conn.read_nonblock(&mut sink) {
            Ok(0) => return false,
            Ok(_) => continue,
            Err(SyscallError::WouldBlock) => return true,
            Err(_) => return false,
        }
    }
}

/// Answer one connection's first frame: a listing, or an open.
fn handshake(service: &mut Service, p: Pending, msg_type: u32, payload_len: usize) {
    let refuse = |conn: &Connection, why: Refusal| {
        let _ = conn.try_send_bytes(wire::MSG_REFUSED, &why.encode());
    };
    if msg_type == wire::MSG_LIST && payload_len == 0 {
        match service.listing() {
            Err(why) => refuse(&p.conn, why),
            // One frame, and the connection is done with: a table larger than
            // a frame is one this service lists no part of rather than half of.
            Ok(listing) => {
                if p.conn.try_send_bytes(wire::MSG_LISTED, &listing).is_err() {
                    refuse(&p.conn, Refusal::Exhausted);
                }
            }
        }
        return;
    }
    let guid = wire::guid(p.rx.payload(payload_len));
    let handles = p.conn.recv_handles_exact::<1>();
    let (Some(guid), Some([region]), wire::MSG_OPEN) = (guid, handles, msg_type) else {
        if let Some([region]) = handles {
            toyos_abi::syscall::close(region);
        }
        refuse(&p.conn, Refusal::Malformed);
        return;
    };
    match service.open(guid, region) {
        Err(why) => {
            println!("blockd: an open of {} refused: {why:?}", guid_text(guid));
            refuse(&p.conn, why);
        }
        Ok(opening) => {
            let opened = Opened { blocks: opening.blocks, unique: guid };
            if p.conn.try_send_bytes(wire::MSG_OPENED, &opened.encode()).is_err() {
                service.abandon(opening);
                return;
            }
            let id = service.admit(opening, p.conn);
            println!("blockd: session {id} opened {} ({} blocks)", guid_text(guid), opened.blocks);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A controller this service cannot use, or one whose claim was refused,
    /// is a disk there and never a machine without one: its listing and its
    /// open are refused by that word, where an absent controller's listing is
    /// empty and its open `NotFound`.
    #[test]
    fn an_unusable_or_unclaimed_controller_is_refused_and_an_absent_one_lists_nothing() {
        let guid = [7; 16];
        let mut unusable = Service::new(Drive::Unusable, None);
        assert_eq!(unusable.listing(), Err(Refusal::Unusable));
        assert_eq!(unusable.place(guid), Err(Refusal::Unusable));
        let mut unclaimed = Service::new(Drive::ClaimRefused, None);
        assert_eq!(unclaimed.listing(), Err(Refusal::ClaimRefused));
        assert_eq!(unclaimed.place(guid), Err(Refusal::ClaimRefused));
        let mut absent = Service::new(Drive::Absent, None);
        assert_eq!(absent.listing(), Ok(Vec::new()));
        assert_eq!(absent.place(guid), Err(Refusal::NotFound));
    }
}
