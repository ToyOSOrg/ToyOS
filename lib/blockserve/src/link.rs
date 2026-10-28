//! A disk driver's connection to the block service.
//!
//! **What the service knows of a driver is what the link last said**: its
//! name, each disk it offers, and how its bring-up ended. A service that ends
//! loses all of it, so the link connects again at once — the port queues the
//! connection for the service started in its place — and says everything
//! again. Sessions already handed over are the driver's and go on.
//!
//! **A driver never blocks on the service**: every frame is one `try_send`,
//! and one the service will not take ends the connection, which is connected
//! again and told everything.

use std::collections::BTreeMap;

use toyos::endow::{self, EndowError};
use toyos::ipc::{Connection, FrameRx, RxStep};
use toyos::poller::{Poller, READABLE};
use toyos_blockring::disk::{self, Offer, Part, Settled};

/// What the service handed over, each with the client's connection the
/// driver answers on.
pub enum Handed {
    /// A session to open, over the region the client sent.
    Session { session: disk::Session, conn: Connection, region: toyos::OwnedHandle },
    /// A description of one of this driver's disks.
    Describe { disk: u32, conn: Connection },
    Designate { order: disk::Designation, conn: Connection },
}

/// The longest frame the service sends: a session, a designation, or a disk's number.
const FRAME: usize = max(max(disk::Session::BYTES, disk::Designation::BYTES), 4);

const fn max(a: usize, b: usize) -> usize {
    if a > b { a } else { b }
}

/// Fresh connections in a row that may refuse what the driver says before it
/// gives up: a frame refused on an empty connection is not backpressure.
const CONNECTS: u32 = 3;

pub struct Link {
    label: String,
    /// `None` once the service's port has closed for good.
    conn: Option<Connection>,
    rx: FrameRx<{ FRAME }>,
    offers: BTreeMap<u32, Vec<Part>>,
    settled: Option<Settled>,
}

impl Link {
    /// The link of the driver `label` names; `None` for a process its
    /// manifest gives no `disk`.
    pub fn new(label: String) -> Option<Self> {
        let first = match endow::service(disk::PORT) {
            Err(EndowError::NotEndowed) => return None,
            first => first,
        };
        let mut link = Self { label, conn: None, rx: FrameRx::new(), offers: BTreeMap::new(), settled: None };
        link.connect_from(first);
        Some(link)
    }

    /// Connect, and say everything again; a fresh connection that will not
    /// take it is let go and another made. A service that will not take one
    /// connection's frames on any of them is a refusal that does not change,
    /// and the driver ends by name.
    fn connect(&mut self) {
        self.connect_from(endow::service(disk::PORT));
    }

    /// [`Self::connect`], its first connection's answer already in hand.
    fn connect_from(&mut self, first: Result<Connection, EndowError>) {
        let mut first = Some(first);
        for _ in 0..CONNECTS {
            self.rx = FrameRx::new();
            self.conn = match first.take().unwrap_or_else(|| endow::service(disk::PORT)) {
                Ok(conn) => Some(conn),
                Err(EndowError::ServerGone) => {
                    println!("{}: the block service's port is closed for good; nothing new is served", self.label);
                    return;
                }
                Err(why) => panic!("{}: the manifest gives this driver `{}`, and it answered {why:?}", self.label, disk::PORT),
            };
            if self.tell_everything() {
                return;
            }
            println!("{}: the block service's connection would not take what this driver offers; connecting again", self.label);
        }
        panic!("{}: {CONNECTS} connections to the block service in a row would not take what this driver offers", self.label);
    }

    fn tell_everything(&self) -> bool {
        let mut told = self.send(disk::MSG_DRIVER, self.label.as_bytes().to_vec());
        for (&disk, parts) in &self.offers {
            told &= self.send(disk::MSG_OFFER, Offer { disk, parts: parts.clone() }.encode());
        }
        if let Some(settled) = self.settled {
            told &= self.send(disk::MSG_SETTLED, settled.encode().to_vec());
        }
        told
    }

    fn send(&self, msg_type: u32, payload: Vec<u8>) -> bool {
        self.conn.as_ref().is_some_and(|conn| conn.try_send_bytes(msg_type, &payload).is_ok())
    }

    fn ended(&mut self, why: &str) {
        println!("{}: the block service's connection ended ({why}); connecting again", self.label);
        self.connect();
    }

    fn tell(&mut self, msg_type: u32, payload: Vec<u8>) {
        if self.conn.is_some() && !self.send(msg_type, payload) {
            self.ended("it would not take a frame");
        }
    }

    /// Offer disk `disk` and every partition its table names.
    pub fn offer(&mut self, disk: u32, parts: Vec<Part>) {
        let frame = Offer { disk, parts: parts.clone() }.encode();
        self.offers.insert(disk, parts);
        self.tell(disk::MSG_OFFER, frame);
    }

    /// Disk `disk` left: every session on it has ended.
    pub fn gone(&mut self, disk: u32) {
        if self.offers.remove(&disk).is_some() {
            self.tell(disk::MSG_GONE, disk.to_le_bytes().to_vec());
        }
    }

    /// Every disk present at bring-up is offered, or the controller cannot
    /// be used.
    pub fn settle(&mut self, settled: Settled) {
        assert!(self.settled.is_none(), "{}: settled twice", self.label);
        self.settled = Some(settled);
        self.tell(disk::MSG_SETTLED, settled.encode().to_vec());
    }

    pub fn watch(&self, poller: &Poller, token: u64) {
        if let Some(conn) = &self.conn {
            poller.watch(conn, READABLE, token);
        }
    }

    /// Everything the service handed over since the last call. Anything
    /// else it sends is the service breaking the protocol, which ends the
    /// connection.
    pub fn take(&mut self) -> Vec<Handed> {
        let mut handed = Vec::new();
        loop {
            let Some(conn) = &self.conn else { return handed };
            let why = match self.rx.pump(conn) {
                RxStep::Idle => return handed,
                RxStep::Eof => "its process ended",
                RxStep::Malformed => "a frame this protocol does not make",
                RxStep::Frame { msg_type: disk::MSG_SESSION, payload_len, .. } => {
                    let session = disk::Session::decode(self.rx.payload(payload_len));
                    let handles = self.rx.take_handles::<2>();
                    match (session, handles) {
                        (Some(session), Some([conn, region])) => {
                            let conn = Connection::received(conn);
                            handed.push(Handed::Session { session, conn, region });
                            continue;
                        }
                        (_, Some(_)) => "a session frame this protocol does not make",
                        (_, None) => "a session without its two handles",
                    }
                }
                RxStep::Frame { msg_type: msg_type @ (disk::MSG_DESCRIBE | disk::MSG_DESIGNATE), payload_len, .. } => {
                    let payload = self.rx.payload(payload_len).to_vec();
                    match self.rx.take_handles::<1>() {
                        Some([conn]) => {
                            let conn = Connection::received(conn);
                            let asked = match msg_type {
                                disk::MSG_DESCRIBE => disk::disk(&payload).map(|disk| Handed::Describe { disk, conn }),
                                _ => disk::Designation::decode(&payload).map(|order| Handed::Designate { order, conn }),
                            };
                            match asked {
                                Some(asked) => {
                                    handed.push(asked);
                                    continue;
                                }
                                None => "a description or designation frame this protocol does not make",
                            }
                        }
                        None => "a description or designation without the client's connection",
                    }
                }
                RxStep::Frame { .. } => "a frame this protocol does not make",
            };
            self.ended(why);
        }
    }
}
