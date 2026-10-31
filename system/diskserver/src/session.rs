//! One partition, opened through a block service: the client end of
//! `toyos-blockring`.
//!
//! **What a request means is `toyos_blockring::client::Client`'s to decide**;
//! this is the mapping, the arena and the doorbells around it.
//! A request takes arena blocks only while it is on the wire. A write's blocks
//! are the caller's, shared: the client keeps them with the write until a
//! flush covers it, at most [`KEPT_BLOCKS`] before it flushes for them, and
//! the session copies them into the arena each time the write goes out. A
//! read's data is copied out when it is answered.
//!
//! **A service that ends is survivable and not invisible.** [`Session::wait`]
//! reads every completion the server posted before it hung up, answers what
//! was still on the wire [`Outcome::Refused`], and says the session ended; the
//! caller decides when to [`Session::reconnect`], which reaches the service
//! through the same name, opens the same partition over the same region, and
//! issues every acknowledged write no flush had covered again before anything
//! else. Nothing here restarts a service: whoever holds its claim does.

use std::collections::BTreeMap;

use toyos::ipc::{Connection, Frame, FrameRx};
use toyos::namespace::Namespace;
use toyos::poller::READABLE;
use toyos::wake::{Bell, Waker};
use toyos_abi::syscall::SyscallError;
use toyos_blockdev::SharedBlock;
use toyos_blockring::client::{Ask, Client, Outcome, Placing, Ticket};
use toyos_blockring::entry::Completion;
use toyos_blockring::layout::{self, ClientRings, ARENA, MAX_REQUEST_BLOCKS};
use toyos_blockring::wire::{self, Opened, Refusal};
use toyos_blockring::{Run, BLOCK_BYTES};

use toyos_blockserve::region::Region;

/// The most blocks of acknowledged writes a session keeps before the caller's
/// next write waits for a flush of its own: 32 MiB, no less than a file
/// server cache's dirty bound, so a cache that flushes by that bound is never
/// made to flush by this one.
pub const KEPT_BLOCKS: u64 = 8 * 1024;

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
    /// The open session's doorbells, `None` once it ended.
    bells: Option<Bells>,
    rings: ClientRings,
    client: Client<Vec<SharedBlock>>,
    opened: Opened,
    arena: Arena,
    next_ticket: Ticket,
    pending: BTreeMap<Ticket, Pending>,
    /// Each read on the wire's arena blocks.
    reads: BTreeMap<Ticket, Run>,
    /// The most requests this session has had on the wire at once.
    peak: usize,
}

impl Session {
    /// Open the partition whose unique GUID is `guid` through the service
    /// `names` calls `service`.
    pub fn open(names: Namespace, service: &str, guid: [u8; wire::GUID_BYTES]) -> Result<Self, Error> {
        let region = Region::create().map_err(Error::Kernel)?;
        let rings = layout::client(region.words());
        let (bells, opened) = handshake(&names, service, guid, &region)?;
        let mut client = Client::new(KEPT_BLOCKS);
        client.session_started();
        Ok(Self {
            names,
            service: service.to_string(),
            guid,
            region,
            bells: Some(bells),
            rings,
            client,
            opened,
            arena: Arena::new(),
            next_ticket: 0,
            pending: BTreeMap::new(),
            reads: BTreeMap::new(),
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
    /// until a flush covers the write. With [`KEPT_BLOCKS`] kept, it goes out
    /// after a flush of the session's own.
    pub fn submit_write(&mut self, lba: u64, blocks: &[SharedBlock]) -> Result<Ticket, Unsent> {
        let count = transfer_blocks(blocks.len());
        if self.bells.is_none() {
            return Err(Unsent::Ended);
        }
        let ticket = self.ticket();
        self.client.submit(ticket, Ask::Write { lba, blocks: count, payload: blocks.to_vec() });
        self.pending.insert(ticket, Pending::Write);
        Ok(ticket)
    }

    /// Ask for `blocks` read from `lba`; the answer carries the data.
    pub fn submit_read(&mut self, lba: u64, blocks: u32) -> Result<Ticket, Unsent> {
        transfer_blocks(blocks as usize);
        if self.bells.is_none() {
            return Err(Unsent::Ended);
        }
        let ticket = self.ticket();
        self.client.submit(ticket, Ask::Read { lba, blocks });
        self.pending.insert(ticket, Pending::Read);
        Ok(ticket)
    }

    /// Ask for every write acknowledged so far to be made durable.
    pub fn submit_flush(&mut self) -> Result<Ticket, Unsent> {
        if self.bells.is_none() {
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
        let Self { bells, region, rings, client, arena, reads, .. } = self;
        let Some(bells) = bells else { return };
        let page = region.words();
        let mut place = |p: Placing<'_, Vec<SharedBlock>>| match p {
            Placing::Read { ticket, blocks } => {
                let run = arena.alloc(blocks)?;
                reads.insert(ticket, run);
                Some(run)
            }
            Placing::Write { payload, blocks } => {
                let run = arena.alloc(blocks)?;
                let window = region.arena(&run);
                for (k, block) in payload.iter().enumerate() {
                    window.copy_in(k * BLOCK_BYTES, &***block);
                }
                Some(run)
            }
        };
        loop {
            match rings.0.space(page) {
                Ok(0) => break,
                Ok(_) => {}
                // The server wrote an index no server writes: the session is
                // over, and the next drain is where that is acted on.
                Err(_) => break,
            }
            let Some(request) = client.next_request(&mut place) else { break };
            let pushed = rings.0.push(page, request.encode());
            assert_eq!(pushed, Ok(true), "diskserver: a request past the room just counted");
        }
        let on_the_wire = client.on_the_wire().count();
        if rings.0.publish(page) {
            // A server that has ended is found by the wait.
            bells.ring.wake();
        }
        self.peak = self.peak.max(on_the_wire);
    }

    /// Every answer there is; with `block`, wait until there is at least one
    /// or the service has ended.
    pub fn wait(&mut self, block: bool) -> Waited {
        let mut waited = Waited::default();
        if self.bells.is_none() {
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
            let Some(bells) = &self.bells else {
                waited.ended = true;
                return waited;
            };
            if bells.answered.wait_for(u64::MAX).is_err() {
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
                (Pending::Write | Pending::Flush, _) => None,
            };
            answers.push(Answer { ticket, outcome, data });
        }
        let released: Vec<_> = self.client.take_released().collect();
        for run in released {
            self.arena.release(run);
        }
    }

    fn end(&mut self, waited: &mut Waited) {
        self.bells = None;
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
        assert!(self.bells.is_none(), "diskserver: reconnect while a session is open");
        // Before the region goes to the new server: it must find this end's
        // two indices at zero, as it will set its own.
        self.rings = layout::client(self.region.words());
        let (bells, opened) = handshake(&self.names, &self.service, self.guid, &self.region)?;
        if opened != self.opened {
            return Err(Error::Protocol);
        }
        self.bells = Some(bells);
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

/// The service's answer on `conn`, taken into `rx` once it is there, within
/// [`wire::ANSWER_BOUND_NS`].
pub(crate) fn answer<const MAX: usize>(conn: &Connection, rx: &mut FrameRx<MAX>) -> Result<Frame, Error> {
    if !toyos::poller::wait_one(conn, READABLE, wire::ANSWER_BOUND_NS).map_err(Error::Kernel)? {
        return Err(Error::Unanswered);
    }
    rx.recv(conn).map_err(|e| match e {
        toyos::ipc::IpcError::Malformed => Error::Protocol,
        _ => Error::Ended,
    })
}

/// An open session's two doorbells, which the server made and sent with its
/// answer: the one this end rings for requests, and the one it rings for
/// answers, whose waker going is the server's end.
struct Bells {
    ring: Waker,
    answered: Bell,
}

/// Connect, send the region with an open, and wait for the answer, which
/// carries the session's doorbells: the session is theirs from here, and the
/// connection goes.
fn handshake(
    names: &Namespace,
    service: &str,
    guid: [u8; wire::GUID_BYTES],
    region: &Region,
) -> Result<(Bells, Opened), Error> {
    let conn = connect(names, service)?;
    conn.send_bytes_with(wire::MSG_OPEN, &guid, [region.share().map_err(Error::Kernel)?]).map_err(|_| Error::Ended)?;
    let mut rx = Box::new(FrameRx::<{ toyos::ipc::MAX_FRAME_LEN as usize }>::new());
    let frame = answer(&conn, &mut rx)?;
    match frame.msg_type {
        wire::MSG_OPENED => {
            let [ring, answered] = rx.take_handles::<{ wire::OPENED_HANDLES }>().ok_or(Error::Protocol)?;
            let opened = Opened::decode(rx.payload(frame.payload_len)).ok_or(Error::Protocol)?;
            Ok((Bells { ring: Waker::from(ring), answered: Bell::from(answered) }, opened))
        }
        wire::MSG_REFUSED => Err(Refusal::decode(rx.payload(frame.payload_len)).map_or(Error::Protocol, Error::Refused)),
        _ => Err(Error::Protocol),
    }
}

/// Every partition the service `names` calls `service` serves: what a client
/// that finds its partition by type asks before it opens one.
pub fn list(names: &Namespace, service: &str) -> Result<Vec<wire::Listed>, Error> {
    let conn = connect(names, service)?;
    conn.signal(wire::MSG_LIST).map_err(|_| Error::Ended)?;
    let mut rx = Box::new(FrameRx::<{ toyos::ipc::MAX_FRAME_LEN as usize }>::new());
    let frame = answer(&conn, &mut rx)?;
    let payload = rx.payload(frame.payload_len);
    match frame.msg_type {
        wire::MSG_LISTED => wire::Listed::decode_all(payload).map(Iterator::collect).ok_or(Error::Protocol),
        wire::MSG_REFUSED => Err(Refusal::decode(payload).map_or(Error::Protocol, Error::Refused)),
        _ => Err(Error::Protocol),
    }
}
