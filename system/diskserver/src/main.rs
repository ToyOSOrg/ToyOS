//! diskserver: the block service — every disk a driver offers, listed and
//! opened for the clients its grants admit, and each opened session handed to
//! the driver whose disk it is on.
//!
//! **It drives no device.** It serves `block` to clients and `disk` to disk
//! drivers (`toyos_blockring::disk`), one per controller, each of which reads
//! its own partition table and offers its disks. An open the grant admits is
//! handed whole to the offering driver — the client's connection and the
//! region it sent — and the driver answers it and serves the session, so a
//! block never passes through here.
//!
//! **A connection reaches what its grant says** ([`wire::Grant`]): the badge
//! the supervisor minted its connector with, which the kernel stamps on it and
//! answers this acceptor alone. A listing names the partitions the grant
//! admits, an open of any other is refused `NotGranted` whether or not a disk
//! carries it, a connection with no grant reaches nothing, and a session whose
//! grant does not write is handed over as one, whose driver answers every
//! write `ReadOnly`.
//!
//! **"Not yet" is never answered as "absent".** The supervisor starts the disk
//! drivers before this service and says how many (`--drivers`), and names
//! each disk controller it started no driver on (`--undriven`). An open of a
//! partition no driver offers, and a listing, wait until every driver started
//! at boot has settled and every driver that ended has settled again, or for
//! [`DRIVER_BOUND`]; then a listing names what is offered, and what is not
//! offered is refused by what is known: `Unusable` while a driver cannot use
//! its controller or has not settled, `ClaimRefused` while a controller has
//! no driver, and `NotFound` otherwise.
//!
//! **A server never blocks on a client or a driver.** Accept and the first
//! frame are two events, a frame is buffered until whole, and every answer is
//! one `try_send`.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use toyos::endow;
use toyos::ipc::{self, Connection, RxStep};
use toyos::poller::{Poller, READABLE};
use toyos::{AsHandle, OwnedHandle};
use toyos_abi::syscall::{SyscallError, MAX_BADGE};
use toyos_blockring::disk::{self, Offer, Part, Settled};
use toyos_blockring::wire::{self, Grant, Listed, Refusal};
use toyos_blockring::PORT;
use toyos_blockserve::guid_text;

/// How long a disk driver may take from its start to say how its bring-up
/// ended, and so how long a listing or an open waits for it. Policy: a stick's
/// bring-up is a port's reset and enumeration and a Bulk-Only disk's first
/// commands, each bounded far below this.
const DRIVER_BOUND: Duration = Duration::from_secs(30);

/// Connections accepted and not yet asked, and how long one may stay so.
const MAX_PENDING: usize = 16;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);
/// Requests that wait for a driver at once.
const MAX_WAITING: usize = 16;
/// Disk drivers connected at once.
const MAX_DRIVERS: usize = 16;

/// Argv: the disk drivers the supervisor started before this service, and
/// each disk controller it started none on.
const DRIVERS: &str = "--drivers";
const UNDRIVEN: &str = "--undriven";

const TOKEN_ACCEPT: u64 = 0;
const TOKEN_DRIVERS: u64 = 1;
const TOKEN_PENDING: u64 = 0x1_0000;
const TOKEN_DRIVER: u64 = 0x2_0000;

/// One partition a driver offered, on the disk the service numbered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Offered {
    disk: u32,
    part: Part,
}

/// How a driver, by its name, last said its bring-up ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fate {
    Offered,
    Unusable,
    /// Its connection ended, and no driver of its name has settled since.
    Ended,
}

/// Everything the drivers said, and what the supervisor said of them.
#[derive(Default)]
struct Known {
    /// How many disk drivers were started before this service.
    expected: usize,
    undriven: Vec<String>,
    fates: BTreeMap<String, Fate>,
    offered: Vec<Offered>,
}

impl Known {
    /// Whether nothing a driver will say could change an answer: every driver
    /// started at boot has settled, and no driver is between an end and its
    /// next settling.
    fn settled(&self) -> bool {
        self.fates.len() >= self.expected && !self.fates.values().any(|f| *f == Fate::Ended)
    }

    /// The drivers whose controllers' partitions cannot be said absent.
    fn unsettled(&self) -> Vec<&str> {
        self.fates.iter().filter(|(_, f)| **f != Fate::Offered).map(|(l, _)| l.as_str()).collect()
    }

    /// Why a partition no disk offers is not served, once nothing is awaited.
    fn absent(&self) -> Refusal {
        let missing = self.fates.len() < self.expected;
        if missing || !self.unsettled().is_empty() {
            Refusal::Unusable
        } else if !self.undriven.is_empty() {
            Refusal::ClaimRefused
        } else {
            Refusal::NotFound
        }
    }

    /// What a listing through `grant` answers, or `None` while it waits:
    /// every partition offered that it admits. **The grant is asked first**,
    /// so a connection with none learns nothing.
    fn listing(&self, grant: Option<Grant>, expired: bool) -> Option<Result<Vec<Listed>, Refusal>> {
        let Some(grant) = grant else { return Some(Err(Refusal::NotGranted)) };
        if !self.settled() && !expired {
            return None;
        }
        let listed: Vec<Listed> = self
            .offered
            .iter()
            .filter(|o| grant.admits(o.part.unique, o.part.kind))
            .map(|o| Listed { disk: o.disk, unique: o.part.unique, kind: o.part.kind })
            .collect();
        Some(match self.absent() {
            Refusal::NotFound => Ok(listed),
            _ if !listed.is_empty() => Ok(listed),
            why => Err(why),
        })
    }

    /// Where an open of `guid` through `grant` is served, or its refusal,
    /// or `None` while it waits. **The grant is asked first**, so a
    /// connection learns nothing of a partition it does not reach.
    fn open(&self, guid: [u8; 16], grant: Option<Grant>, expired: bool) -> Option<Result<Offered, Refusal>> {
        let Some(grant) = grant else { return Some(Err(Refusal::NotGranted)) };
        let carrying: Vec<&Offered> = self.offered.iter().filter(|o| o.part.unique == guid && guid != [0; 16]).collect();
        // A partition no disk offers has no type: only a unique grant naming
        // it reaches it, to wait for it or be told why it is not there.
        let kind = carrying.first().map_or([0; 16], |o| o.part.kind);
        if !grant.admits(guid, kind) {
            return Some(Err(Refusal::NotGranted));
        }
        Some(match carrying[..] {
            [] if !self.settled() && !expired => return None,
            [] => Err(self.absent()),
            [one] if one.part.blocks.is_some() => Ok(*one),
            [_] => Err(Refusal::Unusable),
            _ => Err(Refusal::Unusable),
        })
    }
}

/// A connection that has not asked anything yet.
struct Pending {
    conn: Connection,
    rx: ipc::FrameRx<{ wire::GUID_BYTES }>,
    since: Instant,
}

/// What a waiting client asked.
enum Ask {
    List,
    Open { guid: [u8; 16], region: OwnedHandle },
}

/// A client whose question waits for the drivers.
struct Waiting {
    conn: Connection,
    grant: Option<Grant>,
    ask: Ask,
    since: Instant,
}

/// One disk driver's connection.
struct Driver {
    conn: Connection,
    rx: ipc::FrameRx<{ 4 + disk::MAX_PARTS * Part::BYTES }>,
    label: Option<String>,
    settled: bool,
    /// The driver's number for each disk it offers, and the service's.
    disks: BTreeMap<u32, u32>,
    since: Instant,
}

struct Service {
    known: Known,
    drivers: BTreeMap<u64, Driver>,
    next_driver: u64,
    next_disk: u32,
    waiting: Vec<Waiting>,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut known = Known::default();
    let mut at = 1;
    while at < args.len() {
        let value = args.get(at + 1).unwrap_or_else(|| panic!("diskserver: {} takes a value", args[at]));
        match args[at].as_str() {
            DRIVERS => known.expected = value.parse().unwrap_or_else(|_| panic!("diskserver: {DRIVERS} {value}")),
            UNDRIVEN => {
                println!("diskserver: {value} is on this machine and no driver of it started; what it carries is refused");
                known.undriven.push(value.clone());
            }
            other => panic!("diskserver: {other} is no argument of this service"),
        }
        at += 2;
    }
    let clients = endow::acceptor(PORT).unwrap_or_else(|| panic!("diskserver: started serving no `{PORT}` port"));
    let drivers = endow::acceptor(disk::PORT).unwrap_or_else(|| panic!("diskserver: started serving no `{}` port", disk::PORT));
    println!("diskserver: serving the disks of {} driver(s) started before it", known.expected);
    let mut service = Service { known, drivers: BTreeMap::new(), next_driver: 0, next_disk: 0, waiting: Vec::new() };
    service.serve(&clients, &drivers)
}

impl Service {
    fn serve(&mut self, clients: &toyos::port::Acceptor, drivers: &toyos::port::Acceptor) -> ! {
        let poller = Poller::new(2 + MAX_PENDING as u32 + MAX_DRIVERS as u32);
        let mut pending: Vec<Pending> = Vec::new();
        let mut ready: Vec<u64> = Vec::new();
        let mut was_settled = false;
        loop {
            if pending.len() < MAX_PENDING && self.waiting.len() < MAX_WAITING {
                poller.watch(clients, READABLE, TOKEN_ACCEPT);
            }
            if self.drivers.len() < MAX_DRIVERS {
                poller.watch(drivers, READABLE, TOKEN_DRIVERS);
            }
            for p in &pending {
                poller.watch(&p.conn, READABLE, TOKEN_PENDING + u64::from(p.conn.as_handle().0));
            }
            for (id, d) in &self.drivers {
                poller.watch(&d.conn, READABLE, TOKEN_DRIVER + id);
            }
            let now = Instant::now();
            let deadlines = pending.iter().map(|p| p.since + HANDSHAKE_TIMEOUT);
            let deadlines = deadlines.chain(self.waiting.iter().map(|w| w.since + DRIVER_BOUND));
            let deadlines = deadlines.chain(self.drivers.values().filter(|d| d.label.is_none()).map(|d| d.since + HANDSHAKE_TIMEOUT));
            let timeout = deadlines.min().map_or(u64::MAX, |at| at.saturating_duration_since(now).as_nanos() as u64);
            ready.clear();
            poller.wait(1, timeout, |token| ready.push(token));

            let now = Instant::now();
            if ready.contains(&TOKEN_DRIVERS) {
                match drivers.accept() {
                    Ok(conn) => {
                        let id = self.next_driver;
                        self.next_driver += 1;
                        let d = Driver { conn, rx: ipc::FrameRx::new(), label: None, settled: false, disks: BTreeMap::new(), since: now };
                        self.drivers.insert(id, d);
                    }
                    Err(why) => println!("diskserver: the kernel refused a driver's connection: {why:?}"),
                }
            }
            let ids: Vec<u64> = self.drivers.keys().copied().collect();
            for id in ids {
                let unnamed = self.drivers[&id].label.is_none() && now >= self.drivers[&id].since + HANDSHAKE_TIMEOUT;
                if unnamed {
                    self.drop_driver(id, "it never said its name");
                } else if ready.contains(&(TOKEN_DRIVER + id)) {
                    self.hear_driver(id);
                }
            }

            pending.retain(|p| now < p.since + HANDSHAKE_TIMEOUT);
            if ready.contains(&TOKEN_ACCEPT) {
                match clients.accept() {
                    Ok(conn) => pending.push(Pending { conn, rx: ipc::FrameRx::new(), since: now }),
                    Err(why) => panic!("diskserver: its own acceptor refused an accept: {why:?}"),
                }
            }
            let mut i = 0;
            while i < pending.len() {
                let token = TOKEN_PENDING + u64::from(pending[i].conn.as_handle().0);
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
                        self.asked(clients, p, msg_type, payload_len, now);
                    }
                }
            }

            if self.known.settled() && !was_settled {
                println!("diskserver: every disk driver has settled, {} partition(s) offered", self.known.offered.len());
            }
            was_settled = self.known.settled();
            self.answer_waiting(now);
        }
    }

    /// Everything driver `id` sent; a frame this protocol does not make
    /// ends it.
    fn hear_driver(&mut self, id: u64) {
        loop {
            let d = self.drivers.get_mut(&id).expect("a live driver");
            let (msg_type, payload) = match d.rx.pump(&d.conn) {
                RxStep::Idle => return,
                RxStep::Eof => return self.drop_driver(id, "its connection ended"),
                RxStep::Malformed => return self.drop_driver(id, "a frame this protocol does not make"),
                RxStep::Frame { msg_type, payload_len } => (msg_type, d.rx.payload(payload_len).to_vec()),
            };
            if let Err(why) = self.heard(id, msg_type, &payload) {
                return self.drop_driver(id, why);
            }
        }
    }

    /// One frame of driver `id`'s, or why it ends the driver.
    fn heard(&mut self, id: u64, msg_type: u32, payload: &[u8]) -> Result<(), &'static str> {
        let d = self.drivers.get_mut(&id).expect("a live driver");
        let Some(label) = d.label.clone() else {
            if msg_type != disk::MSG_DRIVER {
                return Err("a frame before its name");
            }
            let label = disk::label(payload).ok_or("a name that is not one")?;
            println!("diskserver: disk driver {label} connected");
            d.label = Some(label);
            return Ok(());
        };
        match msg_type {
            disk::MSG_OFFER => {
                let offer = Offer::decode(payload).ok_or("an offer that is not one")?;
                if d.disks.contains_key(&offer.disk) {
                    return Err("a disk it already offered");
                }
                self.offer(id, offer);
            }
            disk::MSG_SETTLED => {
                let settled = Settled::decode(payload).ok_or("a settling that is not one")?;
                if std::mem::replace(&mut d.settled, true) {
                    return Err("a second settling");
                }
                let fate = match settled {
                    Settled::Offered => Fate::Offered,
                    Settled::Unusable => {
                        println!("diskserver: disk driver {label} cannot use its controller; nothing it would carry is served");
                        Fate::Unusable
                    }
                };
                println!("diskserver: disk driver {label} settled with {} disk(s)", d.disks.len());
                self.known.fates.insert(label, fate);
            }
            disk::MSG_GONE => {
                let ours = disk::disk(payload).and_then(|theirs| d.disks.remove(&theirs));
                let ours = ours.ok_or("the end of a disk it never offered")?;
                self.known.offered.retain(|o| o.disk != ours);
                println!("diskserver: disk {ours} left");
            }
            disk::MSG_DRIVER => return Err("its name a second time"),
            _ => return Err("a frame this protocol does not make"),
        }
        Ok(())
    }

    fn offer(&mut self, id: u64, offer: Offer) {
        let ours = self.next_disk;
        self.next_disk += 1;
        let d = self.drivers.get_mut(&id).expect("a live driver");
        d.disks.insert(offer.disk, ours);
        println!(
            "diskserver: disk {ours}, {}'s disk {}, offers {} partition(s)",
            d.label.as_deref().unwrap_or("?"),
            offer.disk,
            offer.parts.len()
        );
        for part in &offer.parts {
            if self.known.offered.iter().any(|o| o.part.unique == part.unique) {
                println!("diskserver: partition {} is on two disks; neither is served", guid_text(part.unique));
            }
        }
        self.known.offered.extend(offer.parts.into_iter().map(|part| Offered { disk: ours, part }));
    }

    /// Driver `id` is gone, and every disk it offered with it; a driver whose
    /// connection ended is awaited until a driver of its name settles again.
    fn drop_driver(&mut self, id: u64, why: &str) {
        let d = self.drivers.remove(&id).expect("a live driver");
        let label = d.label.unwrap_or_else(|| "an unnamed driver".into());
        println!("diskserver: dropping disk driver {label} — {why}; its {} disk(s) leave", d.disks.len());
        let gone: Vec<u32> = d.disks.values().copied().collect();
        self.known.offered.retain(|o| !gone.contains(&o.disk));
        if self.known.fates.contains_key(&label) {
            self.known.fates.insert(label, Fate::Ended);
        }
    }

    /// One client's first frame: a listing, or an open.
    fn asked(&mut self, clients: &toyos::port::Acceptor, p: Pending, msg_type: u32, payload_len: usize, now: Instant) {
        let grant = grant(clients, &p.conn);
        if msg_type == wire::MSG_LIST && payload_len == 0 {
            return self.waiting.push(Waiting { conn: p.conn, grant, ask: Ask::List, since: now });
        }
        let guid = wire::guid(p.rx.payload(payload_len));
        let handles = p.conn.recv_handles_exact::<1>();
        let (Some(guid), Some([region]), wire::MSG_OPEN) = (guid, handles, msg_type) else {
            if let Some([region]) = handles {
                toyos_abi::syscall::close(region);
            }
            return refuse(&p.conn, Refusal::Malformed);
        };
        // SAFETY: the handle arrived with this frame and nothing else in this
        // process names it.
        let region = unsafe { OwnedHandle::from_raw(region) };
        self.waiting.push(Waiting { conn: p.conn, grant, ask: Ask::Open { guid, region }, since: now });
    }

    /// Answer every waiting question the drivers have now answered, or whose
    /// wait is over.
    fn answer_waiting(&mut self, now: Instant) {
        let waiting = std::mem::take(&mut self.waiting);
        for w in waiting {
            let expired = now >= w.since + DRIVER_BOUND;
            match w.ask {
                Ask::List => match self.known.listing(w.grant, expired) {
                    None => self.waiting.push(w),
                    Some(answer) => {
                        self.waited(expired, "a listing");
                        list(&w.conn, answer);
                    }
                },
                Ask::Open { guid, region } => match self.known.open(guid, w.grant, expired) {
                    None => self.waiting.push(Waiting { ask: Ask::Open { guid, region }, ..w }),
                    Some(Err(why)) => {
                        self.waited(expired, "an open");
                        println!("diskserver: an open of {} refused: {why:?}", guid_text(guid));
                        refuse(&w.conn, why);
                    }
                    Some(Ok(offered)) => {
                        let writes = w.grant.expect("admitted through a grant").writes;
                        self.hand_over(offered, guid, writes, w.conn, region);
                    }
                },
            }
        }
    }

    /// Say which drivers a question answered at its bound waited for.
    fn waited(&self, expired: bool, what: &str) {
        if !expired || self.known.settled() {
            return;
        }
        let started = self.known.expected.saturating_sub(self.known.fates.len());
        println!(
            "diskserver: {what} waited {DRIVER_BOUND:?} and is answered with what is known; not settled: {:?}, and \
             {started} driver(s) started before this service never said who they are",
            self.known.unsettled()
        );
    }

    /// The session goes to the driver whose disk `offered` is: the client's
    /// connection and region, then the frame naming them.
    fn hand_over(&mut self, offered: Offered, guid: [u8; 16], writes: bool, conn: Connection, region: OwnedHandle) {
        let Some((&id, d)) = self.drivers.iter().find(|(_, d)| d.disks.values().any(|&ours| ours == offered.disk)) else {
            unreachable!("diskserver: disk {} is offered and no driver has it", offered.disk)
        };
        let theirs = *d.disks.iter().find(|(_, ours)| **ours == offered.disk).expect("just found").0;
        let session = disk::Session { disk: theirs, unique: guid, writes };
        // The client hears nothing from here: its open is the driver's to
        // answer, and a refused move closes the client's connection.
        if let Err(why) = d.conn.send_handles([conn.into_owned(), region]) {
            return println!("diskserver: the kernel would not move a session to {:?}: {why:?}", d.label);
        }
        // Its handles are in the driver's queue ahead of any later frame's,
        // so a frame the driver will not take ends the driver.
        if d.conn.try_send_bytes(disk::MSG_SESSION, &session.encode()).is_err() {
            return self.drop_driver(id, "it would not take a session");
        }
    }
}

fn refuse(conn: &Connection, why: Refusal) {
    let _ = conn.try_send_bytes(wire::MSG_REFUSED, &why.encode());
}

/// One frame, and the connection is done with: a listing larger than a frame
/// is one this service lists no part of rather than half of.
fn list(conn: &Connection, answer: Result<Vec<Listed>, Refusal>) {
    match answer {
        Err(why) => refuse(conn, why),
        Ok(listed) => {
            let bytes: Vec<u8> = listed.iter().flat_map(Listed::encode).collect();
            if conn.try_send_bytes(wire::MSG_LISTED, &bytes).is_err() {
                refuse(conn, Refusal::Exhausted);
            }
        }
    }
}

/// The grant `conn`, accepted from `acceptor`, was minted with; `None` for a
/// connection through an unbadged connector or with bytes no minter stamps.
fn grant(acceptor: &toyos::port::Acceptor, conn: &Connection) -> Option<Grant> {
    let mut badge = [0u8; MAX_BADGE];
    match acceptor.badge(conn, &mut badge) {
        Ok(bytes) => Grant::decode(bytes),
        Err(SyscallError::NotFound) => None,
        Err(why) => panic!("diskserver: its own acceptor would not say a connection's badge: {why:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use toyos_blockring::wire::Scope;

    const LOG: [u8; 16] = [1; 16];
    const BOOT: [u8; 16] = [2; 16];
    const DATA: [u8; 16] = [3; 16];
    const ROOT: [u8; 16] = [4; 16];
    const LOG_KIND: [u8; 16] = [0x10; 16];
    const BOOT_KIND: [u8; 16] = [0x20; 16];
    const DATA_KIND: [u8; 16] = [0x30; 16];
    const ROOT_KIND: [u8; 16] = [0x40; 16];

    fn unique(guid: [u8; 16]) -> Option<Grant> {
        Some(Grant { scope: Scope::Unique(guid), writes: true })
    }

    fn part(unique: [u8; 16], kind: [u8; 16]) -> Part {
        Part { unique, kind, blocks: Some(100) }
    }

    /// One settled driver offering LOG, BOOT and ROOT on disk 0, and another
    /// offering DATA on disk 1.
    fn two_disks() -> Known {
        let mut known = Known { expected: 2, ..Known::default() };
        known.fates.insert("stick".into(), Fate::Offered);
        known.fates.insert("nvme".into(), Fate::Offered);
        for p in [part(LOG, LOG_KIND), part(BOOT, BOOT_KIND), part(ROOT, ROOT_KIND)] {
            known.offered.push(Offered { disk: 0, part: p });
        }
        known.offered.push(Offered { disk: 1, part: part(DATA, DATA_KIND) });
        known
    }

    /// A unique grant opens its one partition on whichever disk carries it,
    /// and is refused every other by name, as is a connection with no grant;
    /// a kind grant opens every partition of its type and no other.
    #[test]
    fn an_open_reaches_only_what_its_grant_admits() {
        let known = two_disks();
        assert_eq!(known.open(LOG, unique(LOG), false), Some(Ok(Offered { disk: 0, part: part(LOG, LOG_KIND) })));
        for other in [BOOT, DATA, ROOT] {
            assert_eq!(known.open(other, unique(LOG), false), Some(Err(Refusal::NotGranted)));
            assert_eq!(known.open(other, None, false), Some(Err(Refusal::NotGranted)));
        }
        let data = Some(Grant { scope: Scope::Kind(DATA_KIND), writes: true });
        assert_eq!(known.open(DATA, data, false), Some(Ok(Offered { disk: 1, part: part(DATA, DATA_KIND) })));
        assert_eq!(known.open(LOG, data, false), Some(Err(Refusal::NotGranted)));
        assert_eq!(known.open([9; 16], data, false), Some(Err(Refusal::NotGranted)), "nothing of a GUID no disk offers");
        assert_eq!(known.open([9; 16], unique([9; 16]), false), Some(Err(Refusal::NotFound)));
    }

    /// A listing names exactly what its grant admits across every disk, and
    /// nothing to a connection with none.
    #[test]
    fn a_listing_names_only_what_its_grant_admits() {
        let known = two_disks();
        let uniques = |answer: Option<Result<Vec<Listed>, Refusal>>| {
            answer.expect("answered").map(|all| all.iter().map(|l| (l.disk, l.unique)).collect::<Vec<_>>())
        };
        assert_eq!(uniques(known.listing(unique(BOOT), false)), Ok(vec![(0, BOOT)]));
        let data = Some(Grant { scope: Scope::Kind(DATA_KIND), writes: false });
        assert_eq!(uniques(known.listing(data, false)), Ok(vec![(1, DATA)]));
        assert_eq!(uniques(known.listing(unique([9; 16]), false)), Ok(vec![]));
        assert_eq!(known.listing(None, false), Some(Err(Refusal::NotGranted)));
    }

    /// Until every driver started at boot has settled, a listing and an open
    /// of what no disk offers wait; an open of what is offered is answered at
    /// once. At the bound each is answered with what is known, and a missing
    /// driver is never taken for a machine without the partition.
    #[test]
    fn what_no_disk_offers_waits_for_every_driver_started_at_boot() {
        let mut known = Known { expected: 2, ..Known::default() };
        known.fates.insert("nvme".into(), Fate::Offered);
        known.offered.push(Offered { disk: 0, part: part(DATA, DATA_KIND) });
        let data = Some(Grant { scope: Scope::Kind(DATA_KIND), writes: true });
        assert_eq!(known.listing(data, false), None, "the stick's driver has not settled");
        assert_eq!(known.open(LOG, unique(LOG), false), None);
        assert!(known.open(DATA, data, false).is_some_and(|r| r.is_ok()), "offered: answered at once");
        assert_eq!(known.open(LOG, unique(LOG), true), Some(Err(Refusal::Unusable)), "the bound passed, a driver missing");
        assert!(known.listing(data, true).is_some_and(|r| r.is_ok_and(|l| l.len() == 1)));
        let log = Some(Grant { scope: Scope::Kind(LOG_KIND), writes: true });
        assert_eq!(known.listing(log, true), Some(Err(Refusal::Unusable)), "nothing listed, a driver missing");

        known.fates.insert("stick".into(), Fate::Offered);
        assert_eq!(known.open(LOG, unique(LOG), false), Some(Err(Refusal::NotFound)), "every driver said");
        assert_eq!(known.listing(log, false), Some(Ok(vec![])));
    }

    /// A driver that ended is awaited until one of its name settles again,
    /// as a file server reopening after a driver's restart needs.
    #[test]
    fn a_driver_that_ended_is_awaited_until_it_settles_again() {
        let mut known = two_disks();
        known.fates.insert("stick".into(), Fate::Ended);
        known.offered.retain(|o| o.disk != 0);
        assert_eq!(known.open(LOG, unique(LOG), false), None);
        known.fates.insert("stick".into(), Fate::Offered);
        known.offered.push(Offered { disk: 2, part: part(LOG, LOG_KIND) });
        assert_eq!(known.open(LOG, unique(LOG), false), Some(Ok(Offered { disk: 2, part: part(LOG, LOG_KIND) })));
    }

    /// A controller its driver cannot use, or whose claim was refused, is a
    /// disk there and never a machine without one.
    #[test]
    fn an_unusable_or_unclaimed_controller_is_refused_and_an_absent_one_lists_nothing() {
        let data = Some(Grant { scope: Scope::Kind(DATA_KIND), writes: true });
        let mut unusable = Known { expected: 1, ..Known::default() };
        unusable.fates.insert("nvme".into(), Fate::Unusable);
        assert_eq!(unusable.listing(data, false), Some(Err(Refusal::Unusable)));
        assert_eq!(unusable.open(DATA, unique(DATA), false), Some(Err(Refusal::Unusable)));
        let unclaimed = Known { undriven: vec!["pci:01:08:02".into()], ..Known::default() };
        assert_eq!(unclaimed.listing(data, false), Some(Err(Refusal::ClaimRefused)));
        assert_eq!(unclaimed.open(DATA, unique(DATA), false), Some(Err(Refusal::ClaimRefused)));
        let absent = Known::default();
        assert_eq!(absent.listing(data, false), Some(Ok(vec![])));
        assert_eq!(absent.open(DATA, unique(DATA), false), Some(Err(Refusal::NotFound)));
    }

    /// A partition two disks offer, or one its driver will not serve, is
    /// listed and refused `Unusable`.
    #[test]
    fn a_partition_on_two_disks_or_unserved_is_unusable() {
        let mut known = two_disks();
        known.offered.push(Offered { disk: 1, part: part(LOG, LOG_KIND) });
        known.offered.push(Offered { disk: 1, part: Part { unique: [7; 16], kind: LOG_KIND, blocks: None } });
        assert_eq!(known.open(LOG, unique(LOG), false), Some(Err(Refusal::Unusable)));
        assert_eq!(known.open([7; 16], unique([7; 16]), false), Some(Err(Refusal::Unusable)));
    }
}
