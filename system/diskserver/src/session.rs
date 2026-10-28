//! One partition, opened through a block service: the client end of
//! `toyos-blockring`.
//!
//! **What a request means is `toyos_blockring::client::Client`'s to decide**;
//! this is the mapping, the arena, the doorbell and the connection around it.
//! A request takes arena blocks only while it is on the wire. A write's blocks
//! are the caller's, shared: the session keeps them from submit until a flush
//! covers the write, because after a loss they are the only copy there is,
//! and copies them into the arena each time the write goes out. A read's data
//! is copied out when it is answered.
//!
//! **A service that ends is survivable and not invisible.** [`Session::wait`]
//! reads every completion the server posted before it hung up, answers what
//! was still on the wire [`Outcome::Refused`], and says the session ended; the
//! caller decides when to [`Session::reconnect`], which reaches the service
//! through the same name, opens the same partition over the same region, and
//! issues every acknowledged write no flush had covered again before anything
//! else. Nothing here restarts a service: whoever holds its claim does.

use std::collections::BTreeMap;

use toyos::ipc::Connection;
use toyos::namespace::Namespace;
use toyos::poller::{Poller, READABLE};
use toyos_abi::syscall::SyscallError;
use toyos_blockdev::SharedBlock;
use toyos_blockring::client::{Ask, Client, Outcome, Placing, Ticket};
use toyos_blockring::entry::Completion;
use toyos_blockring::layout::{self, ClientRings, ARENA, MAX_REQUEST_BLOCKS};
use toyos_blockring::wire::{self, Opened, Refusal};
use toyos_blockring::{Run, BLOCK_BYTES};

use toyos_blockserve::region::Region;

/// Why a session could not be opened or go on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The service refused, in its own word.
    Refused(Refusal),
    /// The service hung up before it answered.
    Ended,
    /// This process holds no connector by the service's name.
    NotHeld,
    /// The kernel refused a call the session cannot go on without.
    Kernel(SyscallError),
    /// The service said something this protocol does not.
    Protocol,
    /// No answer came within [`wire::ANSWER_BOUND_NS`].
    Unanswered,
}

/// One ticket's answer, and a read's data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answer {
    pub ticket: Ticket,
    pub outcome: Outcome,
    pub data: Option<Vec<u8>>,
}

/// What [`Session::wait`] found.
#[derive(Debug, Default)]
pub struct Waited {
    pub answers: Vec<Answer>,
    /// The service is gone; [`Session::reconnect`] is the only way on.
    pub ended: bool,
}

/// Why a request was not taken.
///
/// **Nothing is taken while the session is ended**: a request asked for then
/// would go out after the reconnect, after requests asked for later, and the
/// caller that gave up on it would find it done under writes it made since.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unsent {
    /// The session has ended: [`Session::reconnect`] first.
    Ended,
}

/// The arena's blocks, first fit.
struct Arena {
    free: Vec<bool>,
}

impl Arena {
    fn new() -> Self {
        Self { free: vec![true; ARENA.slots() as usize] }
    }

    fn alloc(&mut self, blocks: u32) -> Option<Run> {
        let n = blocks as usize;
        let mut run = 0;
        for i in 0..self.free.len() {
            run = if self.free[i] { run + 1 } else { 0 };
            if run == n {
                let first = i + 1 - n;
                self.free[first..=i].iter_mut().for_each(|b| *b = false);
                return ARENA.run(first as u32, blocks);
            }
        }
        None
    }

    fn release(&mut self, run: Run) {
        let first = run.first() as usize;
        for b in &mut self.free[first..first + run.count() as usize] {
            assert!(!*b, "diskserver: arena run {run:?} released twice");
            *b = true;
        }
    }
}

enum Pending {
    Read,
    Write,
    Flush,
}

pub struct Session {
    names: Namespace,
    service: String,
    guid: [u8; wire::GUID_BYTES],
    region: Region,
    conn: Option<Connection>,
    rings: ClientRings,
    client: Client,
    opened: Opened,
    arena: Arena,
    next_ticket: Ticket,
    pending: BTreeMap<Ticket, Pending>,
    /// Each write's blocks, until it is answered anything but done or the
    /// client retires it.
    held: BTreeMap<Ticket, Vec<SharedBlock>>,
    /// Each read on the wire's arena blocks.
    reads: BTreeMap<Ticket, Run>,
    poller: Poller,
    /// The most requests this session has had on the wire at once.
    peak: usize,
}

impl Session {
    /// Open the partition whose unique GUID is `guid` through the service
    /// `names` calls `service`.
    pub fn open(names: Namespace, service: &str, guid: [u8; wire::GUID_BYTES]) -> Result<Self, Error> {
        let region = Region::create().map_err(Error::Kernel)?;
        let rings = layout::client(region.words());
        let (conn, opened) = handshake(&names, service, guid, &region)?;
        let mut client = Client::new();
        client.session_started();
        Ok(Self {
            names,
            service: service.to_string(),
            guid,
            region,
            conn: Some(conn),
            rings,
            client,
            opened,
            arena: Arena::new(),
            next_ticket: 0,
            pending: BTreeMap::new(),
            held: BTreeMap::new(),
            reads: BTreeMap::new(),
            poller: Poller::new(1),
            peak: 0,
        })
    }

    /// The partition's length in blocks.
    pub fn blocks(&self) -> u64 {
        self.opened.blocks()
    }

    /// The most requests this session has had on the wire at once.
    pub fn peak_on_the_wire(&self) -> usize {
        self.peak
    }

    /// How many acknowledged writes have gone out again after a loss.
    pub fn reissued(&self) -> u64 {
        self.client.reissued()
    }

    fn ticket(&mut self) -> Ticket {
        let t = self.next_ticket;
        self.next_ticket += 1;
        t
    }

    /// Ask for `blocks` written at `lba`, which the session keeps a share of
    /// until a flush covers the write.
    pub fn submit_write(&mut self, lba: u64, blocks: &[SharedBlock]) -> Result<Ticket, Unsent> {
        let count = transfer_blocks(blocks.len());
        if self.conn.is_none() {
            return Err(Unsent::Ended);
        }
        let ticket = self.ticket();
        self.held.insert(ticket, blocks.to_vec());
        self.client.submit(ticket, Ask::Write { lba, blocks: count });
        self.pending.insert(ticket, Pending::Write);
        Ok(ticket)
    }

    /// Ask for `blocks` read from `lba`; the answer carries the data.
    pub fn submit_read(&mut self, lba: u64, blocks: u32) -> Result<Ticket, Unsent> {
        transfer_blocks(blocks as usize);
        if self.conn.is_none() {
            return Err(Unsent::Ended);
        }
        let ticket = self.ticket();
        self.client.submit(ticket, Ask::Read { lba, blocks });
        self.pending.insert(ticket, Pending::Read);
        Ok(ticket)
    }

    /// Ask for every write acknowledged so far to be made durable.
    pub fn submit_flush(&mut self) -> Result<Ticket, Unsent> {
        if self.conn.is_none() {
            return Err(Unsent::Ended);
        }
        let ticket = self.ticket();
        self.client.submit(ticket, Ask::Flush);
        self.pending.insert(ticket, Pending::Flush);
        Ok(ticket)
    }

    /// Put what the client will send on the ring, and ring the doorbell once
    /// for all of it.
    fn pump(&mut self) {
        let Self { conn, region, rings, client, arena, held, reads, .. } = self;
        if conn.is_none() {
            return;
        }
        let page = region.words();
        let mut place = |p: Placing| {
            let run = arena.alloc(p.blocks)?;
            if p.write {
                let blocks = held.get(&p.ticket).expect("diskserver: a write going out whose blocks are gone");
                let window = region.arena(&run);
                for (k, block) in blocks.iter().enumerate() {
                    window.copy_in(k * BLOCK_BYTES, &***block);
                }
            } else {
                reads.insert(p.ticket, run);
            }
            Some(run)
        };
        loop {
            match rings.0.space(page) {
                Ok(0) => break,
                Ok(_) => {}
                // The server wrote an index no server writes: the session is
                // over, and the next read of the connection is where that is
                // acted on.
                Err(_) => break,
            }
            let Some(request) = client.next_request(&mut place) else { break };
            let pushed = rings.0.push(page, request.encode());
            assert_eq!(pushed, Ok(true), "diskserver: a request past the room just counted");
        }
        self.peak = self.peak.max(self.client.on_the_wire().count());
        let Some(conn) = &self.conn else { return };
        if self.rings.0.publish(page) {
            // A full pipe is a doorbell already rung; a gone one is a server
            // that has ended, which the wait finds.
            let _ = conn.write_nonblock(&[1]);
        }
    }

    /// Every answer there is; with `block`, wait until there is at least one
    /// or the service has ended.
    pub fn wait(&mut self, block: bool) -> Waited {
        let mut waited = Waited::default();
        if self.conn.is_none() {
            waited.ended = true;
            return waited;
        }
        loop {
            self.pump();
            let violated = self.drain();
            self.collect(&mut waited.answers);
            if violated {
                self.end(&mut waited);
                return waited;
            }
            // Again after every completion: one can make a request sendable
            // that was not — a flush waiting on writes going out again — and
            // a wait with it unsent would wait for an answer nobody asked for.
            self.pump();
            if !waited.answers.is_empty() || !block {
                return waited;
            }
            let Some(conn) = &self.conn else {
                waited.ended = true;
                return waited;
            };
            self.poller.watch(conn, READABLE, 0);
            self.poller.wait(1, u64::MAX, |_| {});
            if !self.take_doorbells() {
                // What the server posted before it went is real: read it
                // before the rest is answered refused.
                self.drain();
                self.collect(&mut waited.answers);
                self.end(&mut waited);
                return waited;
            }
        }
    }

    /// Read every completion posted; `true` if the server broke the protocol.
    fn drain(&mut self) -> bool {
        let page = self.region.words();
        let mut violated = false;
        loop {
            match self.rings.1.pop(page) {
                Ok(Some(words)) => match Completion::decode(words) {
                    Some(c) if self.client.complete(c).is_ok() => {}
                    _ => {
                        violated = true;
                        break;
                    }
                },
                Ok(None) => break,
                Err(_) => {
                    violated = true;
                    break;
                }
            }
        }
        self.rings.1.release(page);
        violated
    }

    /// Consume the doorbell bytes; `false` if the server has hung up.
    fn take_doorbells(&mut self) -> bool {
        let Some(conn) = &self.conn else { return false };
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

    fn collect(&mut self, answers: &mut Vec<Answer>) {
        let decided: Vec<_> = self.client.take_answers().collect();
        for (ticket, outcome) in decided {
            let pending = self.pending.remove(&ticket).expect("diskserver: an answer for no ticket");
            let data = match (pending, outcome) {
                (Pending::Read, outcome) => {
                    let run = self.reads.remove(&ticket);
                    match (run, outcome) {
                        (Some(run), Outcome::Done) => {
                            let mut data = vec![0u8; run.span().len];
                            self.region.arena(&run).copy_out(0, &mut data);
                            Some(data)
                        }
                        _ => None,
                    }
                }
                (Pending::Write, Outcome::Done) => None,
                // Refused, or never taken: nothing will send it again.
                (Pending::Write, _) => {
                    self.held.remove(&ticket);
                    None
                }
                (Pending::Flush, _) => None,
            };
            answers.push(Answer { ticket, outcome, data });
        }
        let retired: Vec<_> = self.client.take_retired().collect();
        for ticket in retired {
            self.held.remove(&ticket);
        }
        let released: Vec<_> = self.client.take_released().collect();
        for run in released {
            self.arena.release(run);
        }
    }

    fn end(&mut self, waited: &mut Waited) {
        self.conn = None;
        self.client.session_ended();
        self.collect(&mut waited.answers);
        waited.ended = true;
    }

    /// Open the same partition over the same region through the same name,
    /// after the service ended; writes the client holds go out again first.
    ///
    /// Blocks until the service answers, within [`wire::ANSWER_BOUND_NS`]: a
    /// name whose server is being restarted queues the connection until the
    /// new one accepts it.
    pub fn reconnect(&mut self) -> Result<(), Error> {
        assert!(self.conn.is_none(), "diskserver: reconnect while a session is open");
        // Before the region goes to the new server: it must find this end's
        // two indices at zero, as it will set its own.
        self.rings = layout::client(self.region.words());
        let (conn, opened) = handshake(&self.names, &self.service, self.guid, &self.region)?;
        if opened != self.opened {
            return Err(Error::Protocol);
        }
        self.conn = Some(conn);
        self.client.session_started();
        self.pump();
        Ok(())
    }
}

fn transfer_blocks(blocks: usize) -> u32 {
    assert!((1..=MAX_REQUEST_BLOCKS as usize).contains(&blocks), "diskserver: a transfer of {blocks} blocks");
    blocks as u32
}

pub(crate) fn connect(names: &Namespace, service: &str) -> Result<Connection, Error> {
    names.open(service).map_err(|e| match e {
        SyscallError::NotFound => Error::NotHeld,
        e => Error::Kernel(e),
    })
}

/// The header of the service's answer on `conn`, once it is there, within
/// [`wire::ANSWER_BOUND_NS`].
pub(crate) fn answer(conn: &Connection) -> Result<toyos::ipc::IpcHeader, Error> {
    let poller = Poller::new(1);
    poller.watch(conn, READABLE, 0);
    let mut ready = false;
    poller.wait(1, wire::ANSWER_BOUND_NS, |_| ready = true);
    if !ready {
        return Err(Error::Unanswered);
    }
    conn.recv_header().map_err(|_| Error::Ended)
}

/// Connect, send the region with an open, and wait for the answer.
fn handshake(
    names: &Namespace,
    service: &str,
    guid: [u8; wire::GUID_BYTES],
    region: &Region,
) -> Result<(Connection, Opened), Error> {
    let conn = connect(names, service)?;
    conn.send_bytes_with(wire::MSG_OPEN, &guid, [region.share().map_err(Error::Kernel)?]).map_err(|_| Error::Ended)?;
    let header = answer(&conn)?;
    let mut payload = [0u8; Opened::BYTES];
    let len = conn.recv_bytes(&header, &mut payload).map_err(|_| Error::Ended)?;
    match header.msg_type {
        wire::MSG_OPENED => Opened::decode(&payload[..len]).map(|o| (conn, o)).ok_or(Error::Protocol),
        wire::MSG_REFUSED => Err(Refusal::decode(&payload[..len]).map_or(Error::Protocol, Error::Refused)),
        _ => Err(Error::Protocol),
    }
}

/// Every partition the service `names` calls `service` serves: what a client
/// that finds its partition by type asks before it opens one.
pub fn list(names: &Namespace, service: &str) -> Result<Vec<wire::Listed>, Error> {
    let conn = connect(names, service)?;
    conn.signal(wire::MSG_LIST).map_err(|_| Error::Ended)?;
    let header = answer(&conn)?;
    let mut payload = vec![0u8; header.len() as usize];
    let len = conn.recv_bytes(&header, &mut payload).map_err(|_| Error::Ended)?;
    match header.msg_type {
        wire::MSG_LISTED => {
            wire::Listed::decode_all(&payload[..len]).map(Iterator::collect).ok_or(Error::Protocol)
        }
        wire::MSG_REFUSED => Err(Refusal::decode(&payload[..len]).map_or(Error::Protocol, Error::Refused)),
        _ => Err(Error::Protocol),
    }
}
