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
//! **A connection reaches what its badge says** ([`wire::Badge`]), minted by
//! the supervisor, which the kernel stamps on it and answers this acceptor
//! alone. A partitions grant lists and opens what it admits, and an open of
//! any other is refused `NotGranted` whether or not a disk carries it; a
//! session whose grant does not write is handed over as one, whose driver
//! answers every write `ReadOnly`. A tables badge lists every partition and
//! has a disk described by its driver, and opens nothing. A designation badge
//! does one thing: the machine's one DATA partition, resolved when it is
//! asked, is designated by the driver of its disk, told whether the running
//! system is on it (`--active`). A connection with no badge reaches nothing.
//!
//! **A driver is who the supervisor says it is**: the badge it minted the
//! driver's `disk` connector with. It starts the disk drivers before this
//! service and names each by that badge (`--driver`), and names each disk
//! controller it started no driver on (`--undriven`).
//!
//! **Nothing is answered while a driver may still change the answer.** A
//! listing and an open wait until every driver started before this service
//! has settled and every driver that ended has settled again, or until
//! [`DRIVER_BOUND`] has passed since this service started or a settled driver
//! last ended; then a listing names what is offered, a GUID two disks offer is
//! refused on both, and what is not offered is refused by what is known:
//! `Unusable` while a driver cannot use its controller or has not settled,
//! `ClaimRefused` while a controller has no driver, and `NotFound` otherwise.
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
use toyos_blockring::wire::{self, Badge, Grant, Listed, Refusal};
use toyos_blockring::PORT;
use toyos_blockserve::guid_text;

/// How long, from this service's start or a settled driver's end, every
/// driver is waited for before a question is answered with what is known;
/// past it every answer is immediate. A client waits twice this for an
/// answer (`wire::ANSWER_BOUND_NS`), which leaves the driver room for its own.
const DRIVER_BOUND: Duration = Duration::from_nanos(wire::DRIVER_BOUND_NS);

/// Connections accepted and not yet asked, and how long one may stay so.
const MAX_PENDING: usize = 16;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);
/// Requests that wait for a driver at once.
const MAX_WAITING: usize = 16;
/// Disk drivers connected at once.
const MAX_DRIVERS: usize = 16;

/// Argv: each disk driver the supervisor started before this service, by the
/// badge of its `disk` connector, and each disk controller it started none
/// on.
const DRIVER: &str = "--driver";
const UNDRIVEN: &str = "--undriven";
/// Argv: a partition the running system is on, by unique GUID, which no
/// designation changes; or that which they are is unknown, and every
/// partition is taken for one.
const ACTIVE: &str = "--active";
const ACTIVE_UNKNOWN: &str = "--active-unknown";

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

/// How a driver last said its bring-up ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fate {
    Offered,
    Unusable,
    /// Its connection ended, and it has not settled since.
    Ended,
}

/// A disk driver as the supervisor named it: the badge it minted the
/// driver's `disk` connector with, which the kernel stamps on the connection
/// and the driver cannot choose. What a driver calls itself is for the lines.
type DriverBadge = u32;

/// Everything the drivers said, and what the supervisor said of them.
#[derive(Default)]
struct Known {
    /// The disk drivers the supervisor started before this service.
    expected: Vec<DriverBadge>,
    undriven: Vec<String>,
    fates: BTreeMap<DriverBadge, Fate>,
    offered: Vec<Offered>,
    /// The partitions the running system is on, or `None` where the
    /// supervisor could not say.
    active: Option<Vec<[u8; 16]>>,
}

impl Known {
    /// Whether nothing a driver will say could change an answer: every driver
    /// started before this service has settled, and no driver is between an
    /// end and its next settling.
    fn settled(&self) -> bool {
        self.expected.iter().all(|d| self.fates.contains_key(d)) && !self.fates.values().any(|f| *f == Fate::Ended)
    }

    /// Driver `badge`'s connection ended. One that had settled is awaited until
    /// it settles again (`true`, so the bound counts from now). One that never
    /// settled is unusable at once: its end is its last word, and every
    /// question waiting on it is answered now rather than at the bound.
    fn ended(&mut self, badge: DriverBadge) -> bool {
        match self.fates.insert(badge, Fate::Ended) {
            Some(_) => true,
            None => {
                self.fates.insert(badge, Fate::Unusable);
                false
            }
        }
    }

    /// The drivers whose controllers' partitions cannot be said absent: those
    /// expected and never heard, and those unusable or ended.
    fn unsettled(&self) -> Vec<DriverBadge> {
        let silent = self.expected.iter().filter(|d| !self.fates.contains_key(d)).copied();
        silent.chain(self.fates.iter().filter(|(_, f)| **f != Fate::Offered).map(|(d, _)| *d)).collect()
    }

    /// Why a partition no disk offers is not served, once nothing is awaited.
    fn absent(&self) -> Refusal {
        if !self.unsettled().is_empty() {
            Refusal::Unusable
        } else if !self.undriven.is_empty() {
            Refusal::ClaimRefused
        } else {
            Refusal::NotFound
        }
    }

    /// What a listing through `badge` answers, or `None` while it waits:
    /// every partition offered that it lists. **The badge is asked first**,
    /// so a connection with none learns nothing.
    fn listing(&self, badge: Option<Badge>, expired: bool) -> Option<Result<Vec<Listed>, Refusal>> {
        let Some(badge) = badge.filter(|b| *b != Badge::DesignateData) else {
            return Some(Err(Refusal::NotGranted));
        };
        if !self.settled() && !expired {
            return None;
        }
        let listed: Vec<Listed> = self
            .offered
            .iter()
            .filter(|o| badge.lists(o.part.unique, o.part.kind))
            .map(|o| Listed { disk: o.disk, unique: o.part.unique, kind: o.part.kind })
            .collect();
        Some(match self.absent() {
            Refusal::NotFound => Ok(listed),
            _ if !listed.is_empty() => Ok(listed),
            why => Err(why),
        })
    }

    /// Where an open of `guid` through `grant` is served, or its refusal,
    /// or `None` while it waits.
    ///
    /// **Nothing is routed until every driver has settled**: a disk that has
    /// not offered yet may carry the same GUID, which refuses it on both, and
    /// one that left may be offering again — a partition a type grant reaches
    /// among them. So a connection with no grant is refused at once, and
    /// every other waits and is judged against every offer.
    fn open(&self, guid: [u8; 16], grant: Option<Grant>, expired: bool) -> Option<Result<Offered, Refusal>> {
        let Some(grant) = grant else { return Some(Err(Refusal::NotGranted)) };
        if !self.settled() && !expired {
            return None;
        }
        let carrying: Vec<&Offered> = self.offered.iter().filter(|o| o.part.unique == guid && guid != [0; 16]).collect();
        // A partition no disk offers has no type: only a unique grant naming
        // it reaches it, to be told why it is not there.
        let kind = carrying.first().map_or([0; 16], |o| o.part.kind);
        if !grant.admits(guid, kind) {
            return Some(Err(Refusal::NotGranted));
        }
        Some(match carrying[..] {
            [] => Err(self.absent()),
            [one] if one.part.blocks.is_some() => Ok(*one),
            _ => Err(Refusal::Unusable),
        })
    }
}

impl Known {
    /// Where a description of disk `disk` through `badge` is answered, or its
    /// refusal, or `None` while it waits: only a [`Badge::Tables`] reaches
    /// one.
    fn describe(&self, disk: u32, badge: Option<Badge>, expired: bool) -> Option<Result<u32, Refusal>> {
        if badge != Some(Badge::Tables) {
            return Some(Err(Refusal::NotGranted));
        }
        if !self.settled() && !expired {
            return None;
        }
        Some(if self.offered.iter().any(|o| o.disk == disk) { Ok(disk) } else { Err(self.absent()) })
    }

    /// Where the designation `badge` asks for is carried out — the one DATA
    /// partition offered, resolved now — and whether the running system is on
    /// it; or its refusal, or `None` while it waits. Two DATA partitions are
    /// never guessed between, and a GUID two disks carry is refused on both,
    /// as an open of it is.
    fn designation(&self, badge: Option<Badge>, expired: bool) -> Option<Result<(Offered, bool), Refusal>> {
        if badge != Some(Badge::DesignateData) {
            return Some(Err(Refusal::NotGranted));
        }
        if !self.settled() && !expired {
            return None;
        }
        let data: Vec<&Offered> = self.offered.iter().filter(|o| o.part.kind == toyos_gpt::Guid::TOYOS_DATA.0).collect();
        let Some(one) = data.first().filter(|_| data.len() == 1) else {
            return Some(Err(if data.is_empty() { self.absent() } else { Refusal::Unusable }));
        };
        let unique = one.part.unique;
        let carrying = self.offered.iter().filter(|o| o.part.unique == unique).count();
        Some(match (carrying, one.part.blocks) {
            (1, Some(_)) => Ok((**one, self.active.as_ref().is_none_or(|a| a.contains(&unique)))),
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
    Describe { disk: u32 },
    Designate { commit: bool },
}

/// A client whose question waits for the drivers.
struct Waiting {
    conn: Connection,
    badge: Option<Badge>,
    ask: Ask,
}

/// One disk driver's connection.
struct Driver {
    badge: DriverBadge,
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
    /// What [`DRIVER_BOUND`] is counted from: this service's start, or the
    /// last end of a driver that had settled.
    awaited_since: Instant,
    drivers: BTreeMap<u64, Driver>,
    next_driver: u64,
    next_disk: u32,
    waiting: Vec<Waiting>,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut known = Known::default();
    let mut at = 1;
    known.active = Some(Vec::new());
    while at < args.len() {
        if args[at] == ACTIVE_UNKNOWN {
            println!("diskserver: which partitions the running system is on is unknown; every designation is refused");
            known.active = None;
            at += 1;
            continue;
        }
        let value = args.get(at + 1).unwrap_or_else(|| panic!("diskserver: {} takes a value", args[at]));
        match args[at].as_str() {
            DRIVER => known.expected.push(value.parse().unwrap_or_else(|_| panic!("diskserver: {DRIVER} {value}"))),
            ACTIVE => {
                let guid = toyos_gpt::Guid::parse(value).unwrap_or_else(|| panic!("diskserver: {ACTIVE} {value}"));
                if let Some(active) = &mut known.active {
                    active.push(guid.0);
                }
            }
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
    println!("diskserver: serving the disks of the driver(s) started before it, {:?}", known.expected);
    let mut service = Service {
        known,
        awaited_since: Instant::now(),
        drivers: BTreeMap::new(),
        next_driver: 0,
        next_disk: 0,
        waiting: Vec::new(),
    };
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
                poller.watch(&p.conn, READABLE, TOKEN_PENDING + u64::from(p.conn.as_handle().raw().0));
            }
            for (id, d) in &self.drivers {
                poller.watch(&d.conn, READABLE, TOKEN_DRIVER + id);
            }
            let now = Instant::now();
            let deadlines = pending.iter().map(|p| p.since + HANDSHAKE_TIMEOUT);
            let deadlines = deadlines.chain((!self.waiting.is_empty()).then_some(self.awaited_since + DRIVER_BOUND));
            let deadlines = deadlines.chain(self.drivers.values().filter(|d| d.label.is_none()).map(|d| d.since + HANDSHAKE_TIMEOUT));
            let timeout = deadlines.min().map_or(u64::MAX, |at| at.saturating_duration_since(now).as_nanos() as u64);
            ready.clear();
            poller.wait(1, timeout, |token| ready.push(token));

            let now = Instant::now();
            if ready.contains(&TOKEN_DRIVERS) {
                match drivers.accept() {
                    Ok(conn) => match driver_badge(drivers, &conn) {
                        None => println!("diskserver: a `disk` connection with no badge the supervisor mints is let go"),
                        Some(badge) if self.drivers.values().any(|d| d.badge == badge) => {
                            println!("diskserver: a second connection of disk driver {badge} is let go")
                        }
                        Some(badge) => {
                            let id = self.next_driver;
                            self.next_driver += 1;
                            let rx = ipc::FrameRx::new();
                            let d = Driver { badge, conn, rx, label: None, settled: false, disks: BTreeMap::new(), since: now };
                            self.drivers.insert(id, d);
                        }
                    },
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
                let token = TOKEN_PENDING + u64::from(pending[i].conn.as_handle().raw().0);
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
                    RxStep::Frame { msg_type, payload_len, .. } => {
                        let p = pending.remove(i);
                        self.asked(clients, p, msg_type, payload_len);
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
                RxStep::Frame { msg_type, payload_len, .. } => (msg_type, d.rx.payload(payload_len).to_vec()),
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
            println!("diskserver: disk driver {} ({label}) connected", d.badge);
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
                println!("diskserver: disk driver {} ({label}) settled with {} disk(s)", d.badge, d.disks.len());
                self.known.fates.insert(d.badge, fate);
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

    /// Driver `id` is gone, and every disk it offered with it ([`Known::ended`]).
    fn drop_driver(&mut self, id: u64, why: &str) {
        let d = self.drivers.remove(&id).expect("a live driver");
        let label = d.label.unwrap_or_else(|| "unnamed".into());
        println!("diskserver: dropping disk driver {} ({label}) — {why}; its {} disk(s) leave", d.badge, d.disks.len());
        let gone: Vec<u32> = d.disks.values().copied().collect();
        self.known.offered.retain(|o| !gone.contains(&o.disk));
        if self.known.ended(d.badge) {
            self.awaited_since = Instant::now();
        } else {
            println!("diskserver: disk driver {} ended before it settled; its controller is unusable", d.badge);
        }
    }

    /// One client's first frame: a listing, an open, a description or a
    /// designation.
    fn asked(&mut self, clients: &toyos::port::Acceptor, mut p: Pending, msg_type: u32, payload_len: usize) {
        let badge = badge(clients, &p.conn);
        let payload = p.rx.payload(payload_len);
        let ask = match (msg_type, payload) {
            (wire::MSG_LIST, []) => Some(Ask::List),
            (wire::MSG_DESCRIBE, _) => disk::disk(payload).map(|disk| Ask::Describe { disk }),
            (wire::MSG_DESIGNATE, [0]) => Some(Ask::Designate { commit: false }),
            (wire::MSG_DESIGNATE, [1]) => Some(Ask::Designate { commit: true }),
            _ => None,
        };
        if let Some(ask) = ask {
            return self.waiting.push(Waiting { conn: p.conn, badge, ask });
        }
        let guid = wire::guid(p.rx.payload(payload_len));
        let handles = p.rx.take_handles::<1>();
        let (Some(guid), Some([region]), wire::MSG_OPEN) = (guid, handles, msg_type) else {
            return refuse(&p.conn, Refusal::Malformed);
        };
        self.waiting.push(Waiting { conn: p.conn, badge, ask: Ask::Open { guid, region } });
    }

    /// Answer every waiting question the drivers have now answered, or whose
    /// wait is over.
    fn answer_waiting(&mut self, now: Instant) {
        let waiting = std::mem::take(&mut self.waiting);
        for w in waiting {
            let expired = now >= self.awaited_since + DRIVER_BOUND;
            match w.ask {
                Ask::List => match self.known.listing(w.badge, expired) {
                    None => self.waiting.push(w),
                    Some(answer) => {
                        self.waited(expired, "a listing");
                        list(&w.conn, answer);
                    }
                },
                Ask::Open { guid, region } => match self.known.open(guid, partitions(w.badge), expired) {
                    None => self.waiting.push(Waiting { ask: Ask::Open { guid, region }, ..w }),
                    Some(Err(why)) => {
                        self.waited(expired, "an open");
                        println!("diskserver: an open of {} refused: {why:?}", guid_text(guid));
                        refuse(&w.conn, why);
                    }
                    Some(Ok(offered)) => {
                        let writes = partitions(w.badge).expect("admitted through a grant").writes;
                        self.hand_over(offered, guid, writes, w.conn, region);
                    }
                },
                Ask::Describe { disk } => match self.known.describe(disk, w.badge, expired) {
                    None => self.waiting.push(Waiting { ask: Ask::Describe { disk }, ..w }),
                    Some(Err(why)) => refuse(&w.conn, why),
                    Some(Ok(ours)) => self.hand_work(ours, disk::MSG_DESCRIBE, |theirs| theirs.to_le_bytes().to_vec(), w.conn),
                },
                Ask::Designate { commit } => match self.known.designation(w.badge, expired) {
                    None => self.waiting.push(Waiting { ask: Ask::Designate { commit }, ..w }),
                    Some(Err(why)) => {
                        println!("diskserver: a designation refused: {why:?}");
                        refuse(&w.conn, why);
                    }
                    Some(Ok((offered, active))) => {
                        let unique = offered.part.unique;
                        let order = |disk| disk::Designation { disk, unique, active, commit }.encode().to_vec();
                        println!("diskserver: designation of {} handed to disk {}'s driver", guid_text(unique), offered.disk);
                        self.hand_work(offered.disk, disk::MSG_DESIGNATE, order, w.conn);
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
        println!(
            "diskserver: {what} is answered with what is known, {DRIVER_BOUND:?} on; not settled: disk driver(s) {:?}",
            self.known.unsettled()
        );
    }

    /// The session goes to the driver whose disk `offered` is: the frame
    /// naming it, carrying the client's connection and region.
    fn hand_over(&mut self, offered: Offered, guid: [u8; 16], writes: bool, conn: Connection, region: OwnedHandle) {
        let Some((&id, d)) = self.drivers.iter().find(|(_, d)| d.disks.values().any(|&ours| ours == offered.disk)) else {
            unreachable!("diskserver: disk {} is offered and no driver has it", offered.disk)
        };
        let theirs = *d.disks.iter().find(|(_, ours)| **ours == offered.disk).expect("just found").0;
        let session = disk::Session { disk: theirs, unique: guid, writes };
        // The client hears nothing from here: its open is the driver's to
        // answer, and a refused send closes the client's connection.
        match d.conn.try_send_bytes_with(disk::MSG_SESSION, &session.encode(), [conn.into_owned(), region]) {
            Ok(()) => {}
            Err(ipc::TrySendError::Full) => self.drop_driver(id, "it would not take a session"),
            Err(why) => println!("diskserver: the kernel would not move a session to disk driver {}: {why:?}", d.badge),
        }
    }
}

impl Service {
    /// Hand `conn` to the driver of disk `ours`, with the frame `payload`
    /// makes of that driver's own number for the disk: the driver answers it.
    fn hand_work(&mut self, ours: u32, msg_type: u32, payload: impl Fn(u32) -> Vec<u8>, conn: Connection) {
        let Some((&id, d)) = self.drivers.iter().find(|(_, d)| d.disks.values().any(|&o| o == ours)) else {
            unreachable!("diskserver: disk {ours} is offered and no driver has it")
        };
        let theirs = *d.disks.iter().find(|(_, o)| **o == ours).expect("just found").0;
        match d.conn.try_send_bytes_with(msg_type, &payload(theirs), [conn.into_owned()]) {
            Ok(()) => {}
            Err(ipc::TrySendError::Full) => self.drop_driver(id, "it would not take a client's work"),
            Err(why) => println!("diskserver: the kernel would not move a client to disk driver {}: {why:?}", d.badge),
        }
    }
}

/// The partitions grant a badge carries, if it is one.
fn partitions(badge: Option<Badge>) -> Option<Grant> {
    match badge {
        Some(Badge::Partitions(grant)) => Some(grant),
        _ => None,
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

/// Who the driver on `conn`, accepted from `acceptor`, is: the badge the
/// supervisor minted its connector with.
fn driver_badge(acceptor: &toyos::port::Acceptor, conn: &Connection) -> Option<DriverBadge> {
    let mut badge = [0u8; MAX_BADGE];
    match acceptor.badge(conn, &mut badge) {
        Ok(bytes) => disk::identity(bytes),
        Err(SyscallError::NotFound) => None,
        Err(why) => panic!("diskserver: its own acceptor would not say a connection's badge: {why:?}"),
    }
}

/// The badge `conn`, accepted from `acceptor`, was minted with; `None` for a
/// connection through an unbadged connector or with bytes no minter stamps.
fn badge(acceptor: &toyos::port::Acceptor, conn: &Connection) -> Option<Badge> {
    let mut badge = [0u8; MAX_BADGE];
    match acceptor.badge(conn, &mut badge) {
        Ok(bytes) => Badge::decode(bytes),
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
    const DATA_KIND: [u8; 16] = toyos_gpt::Guid::TOYOS_DATA.0;
    const ROOT_KIND: [u8; 16] = [0x40; 16];
    /// Two drivers, by the badges the supervisor minted them.
    const STICK: DriverBadge = 7;
    const NVME: DriverBadge = 9;

    fn unique(guid: [u8; 16]) -> Option<Grant> {
        Some(Grant { scope: Scope::Unique(guid), writes: true })
    }

    fn data() -> Option<Grant> {
        Some(Grant { scope: Scope::Kind(DATA_KIND), writes: true })
    }

    fn part(unique: [u8; 16], kind: [u8; 16]) -> Part {
        Part { unique, kind, blocks: Some(100) }
    }

    fn known(expected: &[DriverBadge]) -> Known {
        Known { expected: expected.to_vec(), ..Known::default() }
    }

    /// Both drivers settled: the stick offering LOG, BOOT and ROOT on disk 0,
    /// and the NVMe disk offering DATA on disk 1.
    fn two_disks() -> Known {
        let mut known = known(&[STICK, NVME]);
        known.fates.insert(STICK, Fate::Offered);
        known.fates.insert(NVME, Fate::Offered);
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
        assert_eq!(known.open(DATA, data(), false), Some(Ok(Offered { disk: 1, part: part(DATA, DATA_KIND) })));
        assert_eq!(known.open(LOG, data(), false), Some(Err(Refusal::NotGranted)));
        assert_eq!(known.open([9; 16], data(), false), Some(Err(Refusal::NotGranted)), "nothing of a GUID no disk offers");
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
        assert_eq!(uniques(known.listing(unique(BOOT).map(Badge::Partitions), false)), Ok(vec![(0, BOOT)]));
        assert_eq!(uniques(known.listing(data().map(Badge::Partitions), false)), Ok(vec![(1, DATA)]));
        assert_eq!(uniques(known.listing(unique([9; 16]).map(Badge::Partitions), false)), Ok(vec![]));
        assert_eq!(known.listing(None, false), Some(Err(Refusal::NotGranted)));
    }

    /// Until every driver started before the service has settled, a listing
    /// and every open wait — one the grant admits on a disk already offered
    /// too. At the bound each is answered with what is known, and a missing
    /// driver is never taken for a machine without the partition.
    #[test]
    fn nothing_is_answered_until_every_driver_started_before_the_service_has_settled() {
        let mut known = known(&[STICK, NVME]);
        known.fates.insert(NVME, Fate::Offered);
        known.offered.push(Offered { disk: 0, part: part(DATA, DATA_KIND) });
        assert_eq!(known.listing(data().map(Badge::Partitions), false), None, "the stick's driver has not settled");
        assert_eq!(known.open(LOG, unique(LOG), false), None);
        assert_eq!(known.open(DATA, data(), false), None, "offered, and another disk may carry it too");
        assert_eq!(known.open(DATA, None, false), Some(Err(Refusal::NotGranted)), "no grant waits for nothing");
        assert_eq!(known.open(LOG, unique(LOG), true), Some(Err(Refusal::Unusable)), "the bound passed, a driver silent");
        assert!(known.open(DATA, data(), true).is_some_and(|r| r.is_ok()));
        assert!(known.listing(data().map(Badge::Partitions), true).is_some_and(|r| r.is_ok_and(|l| l.len() == 1)));
        let log = Some(Grant { scope: Scope::Kind(LOG_KIND), writes: true });
        assert_eq!(known.listing(log.map(Badge::Partitions), true), Some(Err(Refusal::Unusable)), "nothing listed, a driver silent");

        known.fates.insert(STICK, Fate::Offered);
        assert_eq!(known.open(LOG, unique(LOG), false), Some(Err(Refusal::NotFound)), "every driver said");
        assert_eq!(known.listing(log.map(Badge::Partitions), false), Some(Ok(vec![])));
    }

    /// A GUID the first driver offers is not routed while another may offer
    /// it too: once the second settles carrying it, it is refused on both,
    /// and no session ever reached either.
    #[test]
    fn a_guid_a_late_disk_also_carries_is_never_routed_to_the_first() {
        let mut known = known(&[STICK, NVME]);
        known.fates.insert(NVME, Fate::Offered);
        known.offered.push(Offered { disk: 0, part: part(LOG, LOG_KIND) });
        assert_eq!(known.open(LOG, unique(LOG), false), None);
        known.fates.insert(STICK, Fate::Offered);
        known.offered.push(Offered { disk: 1, part: part(LOG, LOG_KIND) });
        assert_eq!(known.open(LOG, unique(LOG), false), Some(Err(Refusal::Unusable)));
    }

    /// A driver that ended is awaited until it settles again, whatever the
    /// grant: a file server reopening its type-granted DATA after a driver's
    /// restart waits for the offer rather than being refused for a type no
    /// offer carries any more.
    #[test]
    fn a_driver_that_ended_is_awaited_until_it_settles_again_under_either_grant() {
        let mut known = two_disks();
        known.fates.insert(NVME, Fate::Ended);
        known.offered.retain(|o| o.disk != 1);
        assert_eq!(known.open(DATA, data(), false), None, "a type grant waits for the offer");
        assert_eq!(known.open(DATA, unique(DATA), false), None);
        assert_eq!(known.listing(data().map(Badge::Partitions), false), None);
        known.fates.insert(NVME, Fate::Offered);
        known.offered.push(Offered { disk: 2, part: part(DATA, DATA_KIND) });
        assert_eq!(known.open(DATA, data(), false), Some(Ok(Offered { disk: 2, part: part(DATA, DATA_KIND) })));
    }

    /// A driver is its badge: one the service did not expect, settling, says
    /// nothing of one it expects that never spoke, so the wait goes on; and
    /// two drivers are two whatever they call themselves, since a name never
    /// reaches what is known.
    #[test]
    fn a_late_driver_never_stands_in_for_one_started_before_the_service() {
        let mut known = known(&[STICK, NVME]);
        known.fates.insert(NVME, Fate::Offered);
        known.fates.insert(11, Fate::Offered);
        assert!(!known.settled(), "a late driver counted for the stick's");
        assert_eq!(known.unsettled(), vec![STICK]);
        assert_eq!(known.open(LOG, unique(LOG), false), None);
        known.fates.insert(STICK, Fate::Offered);
        assert!(known.settled());
    }

    /// A driver that ends before it settles holds nothing up: what was waiting
    /// on it is answered at once, `Unusable` for its controller's partitions,
    /// and it is awaited again only once it has settled.
    #[test]
    fn a_driver_that_ends_before_it_settles_is_unusable_at_once() {
        let mut known = known(&[STICK, NVME]);
        known.fates.insert(NVME, Fate::Offered);
        assert_eq!(known.open(LOG, unique(LOG), false), None);
        assert!(!known.ended(STICK), "a driver that never settled was awaited again");
        assert!(known.settled());
        assert_eq!(known.open(LOG, unique(LOG), false), Some(Err(Refusal::Unusable)));
        known.fates.insert(STICK, Fate::Offered);
        assert!(known.ended(STICK), "a settled driver's end is awaited");
        assert!(!known.settled());
    }

    /// A controller its driver cannot use, or one with no driver, is a disk
    /// there and never a machine without one.
    #[test]
    fn an_unusable_or_undriven_controller_is_refused_and_an_absent_one_lists_nothing() {
        let mut unusable = known(&[NVME]);
        unusable.fates.insert(NVME, Fate::Unusable);
        assert_eq!(unusable.listing(data().map(Badge::Partitions), false), Some(Err(Refusal::Unusable)));
        assert_eq!(unusable.open(DATA, unique(DATA), false), Some(Err(Refusal::Unusable)));
        let undriven = Known { undriven: vec!["pci:01:08:02 at PCI 00:02.0".into()], ..Known::default() };
        assert_eq!(undriven.listing(data().map(Badge::Partitions), false), Some(Err(Refusal::ClaimRefused)));
        assert_eq!(undriven.open(DATA, unique(DATA), false), Some(Err(Refusal::ClaimRefused)));
        let absent = Known::default();
        assert_eq!(absent.listing(data().map(Badge::Partitions), false), Some(Ok(vec![])));
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

    /// A description reaches a disk only through a tables badge, and names
    /// one the service has. A designation reaches only the machine's one DATA
    /// partition, resolved when it is asked, said active when the running
    /// system is on it, and is refused when two DATA partitions or two disks
    /// carrying its GUID leave it no one partition; nothing a partitions grant
    /// or a tables badge carries designates anything.
    #[test]
    fn only_its_own_badge_describes_or_designates() {
        let mut known = two_disks();
        known.active = Some(vec![LOG]);
        assert_eq!(known.describe(1, Some(Badge::Tables), false), Some(Ok(1)));
        assert_eq!(known.describe(7, Some(Badge::Tables), false), Some(Err(Refusal::NotFound)));
        for other in [None, data().map(Badge::Partitions), Some(Badge::DesignateData)] {
            assert_eq!(known.describe(1, other, false), Some(Err(Refusal::NotGranted)), "{other:?}");
        }
        let designate = Some(Badge::DesignateData);
        let data_disk = Offered { disk: 1, part: part(DATA, DATA_KIND) };
        assert_eq!(known.designation(designate, false), Some(Ok((data_disk, false))));
        for other in [None, data().map(Badge::Partitions), Some(Badge::Tables)] {
            assert_eq!(known.designation(other, false), Some(Err(Refusal::NotGranted)), "{other:?}");
        }
        assert_eq!(known.listing(designate, false), Some(Err(Refusal::NotGranted)), "a designation lists nothing");
        assert_eq!(known.listing(Some(Badge::Tables), false).map(|r| r.map(|l| l.len())), Some(Ok(4)));

        known.active = Some(vec![DATA]);
        assert_eq!(known.designation(designate, false), Some(Ok((data_disk, true))), "the running system's DATA");
        known.active = None;
        assert_eq!(known.designation(designate, false), Some(Ok((data_disk, true))), "nothing known is active");

        let mut cloned = two_disks();
        cloned.active = Some(Vec::new());
        cloned.offered.push(Offered { disk: 0, part: part([7; 16], LOG_KIND) });
        cloned.offered[0].part.unique = DATA;
        assert_eq!(cloned.designation(designate, false), Some(Err(Refusal::Unusable)), "its GUID on two disks");
        let mut two = two_disks();
        two.offered.push(Offered { disk: 0, part: part([8; 16], DATA_KIND) });
        assert_eq!(two.designation(designate, false), Some(Err(Refusal::Unusable)), "two DATA partitions");
        let mut none = two_disks();
        none.offered.retain(|o| o.part.kind != DATA_KIND);
        assert_eq!(none.designation(designate, false), Some(Err(Refusal::NotFound)));
    }
}
