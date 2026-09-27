//! One partition, opened through a block service: the client end of
//! `toyos-blockring`.
//!
//! **What a request means is `toyos_blockring::client::Client`'s to decide**;
//! this is the mapping, the arena, the doorbell and the connection around it.
//! A write's data is copied into the arena and stays there until a flush
//! covers it, because after a loss it is the only copy there is; a read's is
//! copied out when it is answered.
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
use toyos_blockring::client::{Client, Outcome, Ticket};
use toyos_blockring::entry::{Completion, Op};
use toyos_blockring::layout::{ARENA_BLOCKS, MAX_REQUEST_BLOCKS};
use toyos_blockring::ring::{self, ClientRings};
use toyos_blockring::wire::{self, Opened, Refusal};
use toyos_blockring::BLOCK_BYTES;

use crate::region::Region;

/// Why a session could not be opened or go on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The service refused, in its own word.
    Refused(Refusal),
    /// The service hung up before it answered.
    Ended,
    /// The kernel refused a call the session cannot go on without.
    Kernel(SyscallError),
    /// The service said something this protocol does not.
    Protocol,
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
    /// The arena has no room: a flush releases what acknowledged writes hold.
    ArenaFull,
    /// The session has ended: [`Session::reconnect`] first.
    Ended,
}

/// The arena's blocks, first fit.
struct Arena {
    free: Vec<bool>,
}

impl Arena {
    fn new() -> Self {
        Self { free: vec![true; ARENA_BLOCKS as usize] }
    }

    fn alloc(&mut self, blocks: u32) -> Option<u32> {
        let n = blocks as usize;
        let mut run = 0;
        for i in 0..self.free.len() {
            run = if self.free[i] { run + 1 } else { 0 };
            if run == n {
                let first = i + 1 - n;
                self.free[first..=i].iter_mut().for_each(|b| *b = false);
                return Some(first as u32);
            }
        }
        None
    }

    fn release(&mut self, first: u32, blocks: u32) {
        for b in &mut self.free[first as usize..(first + blocks) as usize] {
            assert!(!*b, "blockd: arena block {first}+{blocks} released twice");
            *b = true;
        }
    }
}

enum Pending {
    Read { arena: u32, blocks: u32 },
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
    poller: Poller,
    /// The most requests this session has had on the wire at once.
    peak: usize,
    /// Answers a synchronous call read on its way to its own, handed out by
    /// the next [`Session::wait`].
    stash: Vec<Answer>,
}

impl Session {
    /// Open the partition whose unique GUID is `guid` through the service
    /// `names` calls `service`.
    pub fn open(names: Namespace, service: &str, guid: [u8; wire::GUID_BYTES]) -> Result<Self, Error> {
        let region = Region::create().map_err(Error::Kernel)?;
        let rings = ring::client(region.words());
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
            poller: Poller::new(1),
            peak: 0,
            stash: Vec::new(),
        })
    }

    /// The partition's length in blocks.
    pub fn blocks(&self) -> u64 {
        self.opened.blocks
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

    /// Ask for `data` (whole blocks) written at `lba`.
    pub fn submit_write(&mut self, lba: u64, data: &[u8]) -> Result<Ticket, Unsent> {
        let blocks = whole_blocks(data.len());
        if self.conn.is_none() {
            return Err(Unsent::Ended);
        }
        let arena = self.arena.alloc(blocks).ok_or(Unsent::ArenaFull)?;
        self.region.arena(arena, blocks).copy_in(0, data);
        let ticket = self.ticket();
        self.client.submit(ticket, Op::Write, lba, blocks, arena);
        self.pending.insert(ticket, Pending::Write);
        Ok(ticket)
    }

    /// Ask for `blocks` read from `lba`; the answer carries the data.
    pub fn submit_read(&mut self, lba: u64, blocks: u32) -> Result<Ticket, Unsent> {
        assert!((1..=MAX_REQUEST_BLOCKS).contains(&blocks), "a read of {blocks} blocks");
        if self.conn.is_none() {
            return Err(Unsent::Ended);
        }
        let arena = self.arena.alloc(blocks).ok_or(Unsent::ArenaFull)?;
        let ticket = self.ticket();
        self.client.submit(ticket, Op::Read, lba, blocks, arena);
        self.pending.insert(ticket, Pending::Read { arena, blocks });
        Ok(ticket)
    }

    /// Ask for every write acknowledged so far to be made durable.
    pub fn submit_flush(&mut self) -> Result<Ticket, Unsent> {
        if self.conn.is_none() {
            return Err(Unsent::Ended);
        }
        let ticket = self.ticket();
        self.client.submit(ticket, Op::Flush, 0, 0, 0);
        self.pending.insert(ticket, Pending::Flush);
        Ok(ticket)
    }

    /// Put what the client will send on the ring, and ring the doorbell once
    /// for all of it.
    fn pump(&mut self) {
        let Some(conn) = &self.conn else { return };
        let page = self.region.words();
        loop {
            match self.rings.0.space(page) {
                Ok(0) => break,
                Ok(_) => {}
                // The server wrote an index no server writes: the session is
                // over, and the next read of the connection is where that is
                // acted on.
                Err(_) => break,
            }
            let Some(request) = self.client.next_request() else { break };
            self.rings.0.push(page, request.encode());
        }
        self.peak = self.peak.max(self.client.on_the_wire());
        if self.rings.0.publish(page) {
            // A full pipe is a doorbell already rung; a gone one is a server
            // that has ended, which the wait finds.
            let _ = conn.write_nonblock(&[1]);
        }
    }

    /// Every answer there is; with `block`, wait until there is at least one
    /// or the service has ended.
    pub fn wait(&mut self, block: bool) -> Waited {
        let mut waited = Waited { answers: core::mem::take(&mut self.stash), ended: false };
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
            let pending = self.pending.remove(&ticket).expect("blockd: an answer for no ticket");
            let data = match (pending, outcome) {
                (Pending::Read { arena, blocks }, Outcome::Done) => {
                    let mut data = vec![0u8; blocks as usize * BLOCK_BYTES];
                    self.region.arena(arena, blocks).copy_out(0, &mut data);
                    Some(data)
                }
                _ => None,
            };
            answers.push(Answer { ticket, outcome, data });
        }
        let released: Vec<_> = self.client.take_released().collect();
        for (first, blocks) in released {
            self.arena.release(first, blocks);
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
    /// Blocks until the service answers: a name whose server is being
    /// restarted queues the connection until the new one accepts it.
    pub fn reconnect(&mut self) -> Result<(), Error> {
        assert!(self.conn.is_none(), "blockd: reconnect while a session is open");
        // Before the region goes to the new server: it must find this end's
        // two indices at zero, as it will set its own.
        self.rings = ring::client(self.region.words());
        let (conn, opened) = handshake(&self.names, &self.service, self.guid, &self.region)?;
        if opened != self.opened {
            return Err(Error::Protocol);
        }
        self.conn = Some(conn);
        self.client.session_started();
        self.pump();
        Ok(())
    }

    /// Wait for one ticket, across nothing: an end is an error here.
    pub fn run(&mut self, ticket: Ticket) -> Result<Answer, Error> {
        loop {
            let waited = self.wait(true);
            let mut found = None;
            for answer in waited.answers {
                if answer.ticket == ticket {
                    found = Some(answer);
                } else {
                    self.stash.push(answer);
                }
            }
            if let Some(answer) = found {
                return Ok(answer);
            }
            if waited.ended {
                return Err(Error::Ended);
            }
        }
    }

    /// Write `data` at `lba` and wait for its answer; a full arena is made
    /// room in by a flush first.
    pub fn write(&mut self, lba: u64, data: &[u8]) -> Result<Outcome, Error> {
        let ticket = match self.submit_write(lba, data) {
            Ok(ticket) => ticket,
            Err(Unsent::Ended) => return Err(Error::Ended),
            Err(Unsent::ArenaFull) => {
                self.flush()?;
                self.submit_write(lba, data).map_err(|_| Error::Ended)?
            }
        };
        self.run(ticket).map(|a| a.outcome)
    }

    /// Read `blocks` from `lba`.
    pub fn read(&mut self, lba: u64, blocks: u32) -> Result<(Outcome, Option<Vec<u8>>), Error> {
        let ticket = match self.submit_read(lba, blocks) {
            Ok(ticket) => ticket,
            Err(Unsent::Ended) => return Err(Error::Ended),
            Err(Unsent::ArenaFull) => {
                self.flush()?;
                self.submit_read(lba, blocks).map_err(|_| Error::Ended)?
            }
        };
        self.run(ticket).map(|a| (a.outcome, a.data))
    }

    /// Flush, and wait for it.
    pub fn flush(&mut self) -> Result<Outcome, Error> {
        let ticket = self.submit_flush().map_err(|_| Error::Ended)?;
        self.run(ticket).map(|a| a.outcome)
    }
}

fn whole_blocks(len: usize) -> u32 {
    assert!(len > 0 && len % BLOCK_BYTES == 0, "blockd: a transfer of {len} bytes is no whole blocks");
    let blocks = (len / BLOCK_BYTES) as u32;
    assert!(blocks <= MAX_REQUEST_BLOCKS, "blockd: a transfer of {blocks} blocks");
    blocks
}

/// Connect, send the region with an open, and wait for the answer.
fn handshake(
    names: &Namespace,
    service: &str,
    guid: [u8; wire::GUID_BYTES],
    region: &Region,
) -> Result<(Connection, Opened), Error> {
    let conn = names.open(service).map_err(Error::Kernel)?;
    let shared = region.share().map_err(Error::Kernel)?;
    conn.send_bytes_with_handles(&[shared], wire::MSG_OPEN, &guid).map_err(|_| Error::Ended)?;
    let header = conn.recv_header().map_err(|_| Error::Ended)?;
    let mut payload = [0u8; Opened::BYTES];
    let len = conn.recv_bytes(&header, &mut payload).map_err(|_| Error::Ended)?;
    match header.msg_type {
        wire::MSG_OPENED => Opened::decode(&payload[..len]).map(|o| (conn, o)).ok_or(Error::Protocol),
        wire::MSG_REFUSED => Err(Refusal::decode(&payload[..len]).map_or(Error::Protocol, Error::Refused)),
        _ => Err(Error::Protocol),
    }
}
