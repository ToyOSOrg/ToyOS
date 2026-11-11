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
//! through the same name, opens the same partition, and issues every
//! acknowledged write no flush had covered again before anything else.
//! Nothing here restarts a service: whoever holds its claim does.
//!
//! **No wait is on the server's word alone.** A server that breaks the
//! protocol, or leaves the session unanswered past the bound every server
//! keeps ([`SILENCE_BOUND_NS`]), is ended as one that hung up is
//! (`toyos_blockring::ends`). That server is still running, and the
//! reconnect reaches it again. One that still holds a request of the ended
//! session refuses the open [`Refusal::Held`] until the request is answered,
//! which is the reconnect failing: the caller's disk is gone and nothing is
//! issued again.
//!
//! **Every session is opened over a region no server has held**, so nothing a
//! server writes to an earlier session's page reaches this one, whether or not
//! that server has let the page go.

use std::collections::BTreeMap;

use toyos::ipc::{Connection, Frame, FrameRx};
use toyos::namespace::Namespace;
use toyos::poller::READABLE;
use toyos::wake::{Bell, Waker};
use toyos_abi::syscall::SyscallError;
use toyos_blockdev::SharedBlock;
use toyos_blockring::client::{Ask, Client, Outcome, Placing, Ticket};
use toyos_blockring::ends::{ClientEnds, Ending, Exchange, Next};
use toyos_blockring::layout::{ARENA, MAX_REQUEST_BLOCKS};
use toyos_blockring::wire::{self, Opened, Refusal};
use toyos_blockring::{Run, BLOCK_BYTES, SILENCE_BOUND_NS};

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
    ends: ClientEnds,
    client: Client<Vec<SharedBlock>>,
    opened: Opened,
    arena: Arena,
    next_ticket: Ticket,
    pending: BTreeMap<Ticket, Pending>,
    /// Each read on the wire's arena blocks.
    reads: BTreeMap<Ticket, Run>,
}

impl Session {
    /// Open the partition whose unique GUID is `guid` through the service
    /// `names` calls `service`.
    pub fn open(names: Namespace, service: &str, guid: [u8; wire::GUID_BYTES]) -> Result<Self, Error> {
        let region = Region::create().map_err(Error::Kernel)?;
        let ends = ClientEnds::new(region.words());
        let (bells, opened) = handshake(&names, service, guid, &region)?;
        let mut client = Client::new(KEPT_BLOCKS);
        client.session_started();
        Ok(Self {
            names,
            service: service.to_string(),
            guid,
            region,
            bells: Some(bells),
            ends,
            client,
            opened,
            arena: Arena::new(),
            next_ticket: 0,
            pending: BTreeMap::new(),
            reads: BTreeMap::new(),
        })
    }

    /// The partition's length in blocks.
    pub fn blocks(&self) -> u64 {
        self.opened.blocks()
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

    /// Exchange requests and completions with the server at `now`
    /// ([`ClientEnds::exchange`]), ring its doorbell once for what went, and
    /// take every answer decided into `answers`.
    fn exchange(&mut self, now: u64, answers: &mut Vec<Answer>) -> Next {
        let Self { bells, region, ends, client, arena, reads, .. } = self;
        let bells = bells.as_ref().expect("diskserver: an exchange over a session that ended");
        let place = |p: Placing<'_, Vec<SharedBlock>>| match p {
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
        let Exchange { next, published } = ends.exchange(region.words(), client, now, place);
        if published {
            // A server that has ended is found by the wait.
            bells.ring.wake();
        }
        self.collect(answers);
        next
    }

    /// Every answer there is; with `block`, wait until there is at least one
    /// or the session has ended, which it has within [`SILENCE_BOUND_NS`] of
    /// the server's last completion.
    pub fn wait(&mut self, block: bool) -> Waited {
        let mut waited = Waited::default();
        if self.bells.is_none() {
            waited.ended = true;
            return waited;
        }
        loop {
            let now = toyos_abi::clock::nanos_since_boot();
            match self.exchange(now, &mut waited.answers) {
                Next::End(Ending::Violation(violation)) => {
                    println!("{}: the session's server broke the protocol ({violation:?})", self.service);
                }
                Next::End(Ending::Silence) => {
                    println!("{}: the session's server answered nothing in {} s", self.service, SILENCE_BOUND_NS / 1_000_000_000);
                }
                Next::Wait(patience) if block => {
                    let bells = self.bells.as_ref().expect("diskserver: the session is open");
                    if bells.answered.wait_for(patience).is_ok() {
                        continue;
                    }
                    // What the server posted before it went is real: read it
                    // before the rest is answered refused.
                    let _ = self.ends.receive(self.region.words(), &mut self.client, now);
                }
                Next::Idle if block => panic!("diskserver: a wait with nothing asked would never end"),
                Next::Answered | Next::Wait(_) | Next::Idle => return waited,
            }
            self.end(&mut waited);
            return waited;
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

    /// Open the same partition through the same name over a fresh region,
    /// after the session ended; at the next [`Self::wait`] the writes the
    /// client holds go out again, first. Refused [`Refusal::Held`] while the
    /// server still holds a request of the session that ended.
    ///
    /// Blocks until the service answers, within [`wire::ANSWER_BOUND_NS`]: a
    /// name whose server is being restarted queues the connection until the
    /// new one accepts it.
    pub fn reconnect(&mut self) -> Result<(), Error> {
        assert!(self.bells.is_none(), "diskserver: reconnect while a session is open");
        let region = Region::create().map_err(Error::Kernel)?;
        let ends = ClientEnds::new(region.words());
        let (bells, opened) = handshake(&self.names, &self.service, self.guid, &region)?;
        if opened != self.opened {
            return Err(Error::Protocol);
        }
        (self.region, self.ends, self.bells) = (region, ends, Some(bells));
        self.client.session_started();
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
