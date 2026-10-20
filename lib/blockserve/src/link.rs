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

/// A session the service handed over: what to open, the client's connection
/// and the region it sent.
pub struct Handed {
    pub session: disk::Session,
    pub conn: Connection,
    pub region: toyos::RawHandle,
}

pub struct Link {
    label: String,
    /// `None` once the service's port has closed for good.
    conn: Option<Connection>,
    rx: FrameRx<{ disk::Session::BYTES }>,
    offers: BTreeMap<u32, Vec<Part>>,
    settled: Option<Settled>,
}

impl Link {
    /// The link of the driver `label` names.
    pub fn new(label: String) -> Self {
        let mut link = Self { label, conn: None, rx: FrameRx::new(), offers: BTreeMap::new(), settled: None };
        link.connect();
        link
    }

    fn connect(&mut self) {
        self.rx = FrameRx::new();
        self.conn = match endow::service(disk::PORT) {
            Ok(conn) => Some(conn),
            Err(EndowError::ServerGone) => {
                println!("{}: the block service's port is closed for good; nothing new is served", self.label);
                return;
            }
            Err(why) => panic!("{}: the manifest gives this driver `{}`, and it answered {why:?}", self.label, disk::PORT),
        };
        let mut told = self.send(disk::MSG_DRIVER, self.label.as_bytes().to_vec());
        let offers: Vec<Vec<u8>> =
            self.offers.iter().map(|(&disk, parts)| Offer { disk, parts: parts.clone() }.encode()).collect();
        for offer in offers {
            told &= self.send(disk::MSG_OFFER, offer);
        }
        if let Some(settled) = self.settled {
            told &= self.send(disk::MSG_SETTLED, settled.encode().to_vec());
        }
        if !told {
            self.ended("it would not take what this driver offers");
        }
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

    /// Every session the service handed over since the last call. Anything
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
                RxStep::Frame { msg_type: disk::MSG_SESSION, payload_len } => {
                    let session = disk::Session::decode(self.rx.payload(payload_len));
                    let handles = conn.recv_handles_exact::<2>();
                    match (session, handles) {
                        (Some(session), Some([conn, region])) => {
                            let conn = Connection::received(conn);
                            handed.push(Handed { session, conn, region });
                            continue;
                        }
                        (_, Some(handles)) => {
                            handles.into_iter().for_each(toyos_abi::syscall::close);
                            "a session frame this protocol does not make"
                        }
                        (_, None) => "a session without its two handles",
                    }
                }
                RxStep::Frame { .. } => "a frame this protocol does not make",
            };
            self.ended(why);
        }
    }
}
