//! What a client decides about its requests: which go on the wire and when,
//! what each completion means to whoever asked, and what has to be written
//! again after a loss.
//!
//! **Every request asked for is answered exactly once**, by [`Client::complete`]
//! or by [`Client::session_ended`], and never twice: a completion for a tag
//! that is not on the wire is the server breaking the session
//! ([`Violation`]), not a second answer.
//!
//! **An acknowledged write is kept until a flush covers it.** Its arena blocks
//! stay pinned ([`Client::take_released`] is when they come back) because a
//! loss — a flush answered [`Status::Lost`] or [`Status::Device`], or a session
//! that ended — means the device may no longer hold it, and the only copy left
//! is the client's. After a loss every such write is issued again, in the order
//! it was first acknowledged and never beside an earlier one it overlaps, and
//! nothing else goes on the wire until they are all acknowledged again; the
//! flush that was waiting is then asked again. A flush is answered
//! [`Outcome::Durable`] only by a device flush that succeeded after every loss
//! it could have been told about.
//!
//! **What was on the wire when a session ended is answered
//! [`Outcome::Refused`]**: a write it took may or may not be on the disk, and
//! the caller is told so rather than told either. What had not yet been put on
//! the wire waits for the next session, unchanged.
//!
//! **Requests in flight at once are unordered.** A caller must not have two in
//! flight whose ranges overlap when either is a write, nor reuse arena blocks
//! before they are released.

use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;

use crate::entry::{Completion, Op, Request, Status};

/// The caller's name for one thing it asked for.
pub type Ticket = u64;

/// How many times a flush is asked again after a loss before its caller is
/// told the device would not make its writes durable, and how many times one
/// write is issued again before the same.
pub const MAX_ATTEMPTS: u32 = 4;

/// What became of one ticket.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Outcome {
    /// A read's data is in its arena blocks; a write was taken by the device,
    /// which is not durable yet.
    Done,
    /// A flush: every write acknowledged before it was asked for is on the
    /// medium.
    Durable,
    /// The server refused it as malformed.
    Invalid,
    /// The device did not do it. A write answered this may or may not have
    /// reached the medium; a flush answered this left writes the device would
    /// not keep.
    Device,
    /// The session ended with it on the wire: it may or may not have happened.
    Refused,
}

/// The server said something no server of this protocol says. The session is
/// over, and [`Client::session_ended`] is what the caller does next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Violation;

/// A write acknowledged and not yet covered by a flush.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Acked {
    lba: u64,
    blocks: u32,
    arena: u32,
    /// When it was first acknowledged, which is the order it is issued again
    /// in.
    first: u64,
    /// When it was last acknowledged, which is what a flush covers by.
    seq: u64,
    attempts: u32,
}

impl Acked {
    fn overlaps_range(&self, lba: u64, blocks: u32) -> bool {
        self.lba < lba + u64::from(blocks) && lba < self.lba + u64::from(self.blocks)
    }
}

/// Why a request is being sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Kind {
    /// The caller asked for it.
    User(Ticket),
    /// An acknowledged write, issued again after a loss.
    Reissue(Acked),
    /// One attempt at the caller's flush `Ticket`.
    Flush(Ticket),
}

/// A request not yet on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Queued {
    op: Op,
    lba: u64,
    blocks: u32,
    arena: u32,
    kind: Kind,
}

/// A request on the wire, and for a flush the acknowledgement it covers below
/// and the losses it was sent after.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Sent {
    queued: Queued,
    covers: u64,
    losses: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct Client {
    up: bool,
    /// Not yet on the wire, in order. Writes being issued again are always at
    /// the front, then any flush attempt waiting on them.
    outbox: VecDeque<Queued>,
    wire: BTreeMap<u32, Sent>,
    acked: VecDeque<Acked>,
    next_tag: u32,
    next_seq: u64,
    /// Losses taken so far: a flush sent before one says nothing about what it
    /// lost.
    losses: u64,
    attempts: BTreeMap<Ticket, u32>,
    answers: VecDeque<(Ticket, Outcome)>,
    released: VecDeque<(u32, u32)>,
    /// Writes put on the wire again after a loss, over the client's life.
    reissued: u64,
}

impl Client {
    /// A client with no session yet: requests wait until
    /// [`Self::session_started`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask for a read or write of `blocks` at `lba`, through arena blocks
    /// from `arena`, or for a flush (whose range is ignored).
    pub fn submit(&mut self, ticket: Ticket, op: Op, lba: u64, blocks: u32, arena: u32) {
        let (lba, blocks, arena) = match op {
            Op::Flush => (0, 0, 0),
            Op::Read | Op::Write => (lba, blocks, arena),
        };
        let kind = match op {
            Op::Flush => Kind::Flush(ticket),
            Op::Read | Op::Write => Kind::User(ticket),
        };
        self.outbox.push_back(Queued { op, lba, blocks, arena, kind });
    }

    fn reissuing(&self) -> bool {
        self.wire.values().any(|sent| matches!(sent.queued.kind, Kind::Reissue(_)))
            || self.outbox.front().is_some_and(|q| matches!(q.kind, Kind::Reissue(_)))
    }

    /// The next request to put on the wire, tagged; `None` while there is no
    /// session, nothing to send, or what is next must wait for writes being
    /// issued again.
    pub fn next_request(&mut self) -> Option<Request> {
        if !self.up {
            return None;
        }
        let front = *self.outbox.front()?;
        match front.kind {
            // Never beside a write on the wire it overlaps: the device may
            // apply two writes in flight at once in either order, and the one
            // on the wire is either an earlier one being issued again or a
            // later one of the caller's.
            Kind::Reissue(acked) => {
                let blocked = self.wire.values().any(|sent| {
                    let q = sent.queued;
                    q.op == Op::Write && acked.overlaps_range(q.lba, q.blocks)
                });
                if blocked {
                    return None;
                }
            }
            Kind::User(_) | Kind::Flush(_) => {
                if self.reissuing() {
                    return None;
                }
            }
        }
        self.outbox.pop_front();
        if matches!(front.kind, Kind::Reissue(_)) {
            self.reissued += 1;
        }
        let tag = self.next_tag;
        self.next_tag = self.next_tag.wrapping_add(1);
        let (covers, losses) = (self.next_seq, self.losses);
        self.wire.insert(tag, Sent { queued: front, covers, losses });
        Some(Request { op: front.op, tag, lba: front.lba, blocks: front.blocks, arena: front.arena })
    }

    /// What the server answered.
    pub fn complete(&mut self, completion: Completion) -> Result<(), Violation> {
        let sent = self.wire.remove(&completion.tag).ok_or(Violation)?;
        let q = sent.queued;
        match (q.kind, completion.status) {
            (Kind::User(ticket), Status::Ok) => {
                match q.op {
                    Op::Write => {
                        let seq = self.bump();
                        let acked =
                            Acked { lba: q.lba, blocks: q.blocks, arena: q.arena, first: seq, seq, attempts: 0 };
                        // Sent before a loss it did not see: the device may
                        // have taken it and lost it, and an earlier write it
                        // overlaps is being issued again — so it is issued
                        // again too, after that one.
                        if sent.losses != self.losses {
                            self.requeue_reissue(acked);
                        } else {
                            self.acked.push_back(acked);
                        }
                    }
                    Op::Read => self.released.push_back((q.arena, q.blocks)),
                    Op::Flush => return Err(Violation),
                }
                self.answers.push_back((ticket, Outcome::Done));
            }
            (Kind::User(ticket), status) => {
                self.released.push_back((q.arena, q.blocks));
                let outcome = match status {
                    Status::Invalid => Outcome::Invalid,
                    Status::Device => Outcome::Device,
                    // A read or a write answered `Lost` is a server that does
                    // not know which op it was.
                    Status::Lost | Status::Ok => return Err(Violation),
                };
                self.answers.push_back((ticket, outcome));
            }
            (Kind::Reissue(mut acked), Status::Ok) => {
                acked.seq = self.bump();
                // A loss since it went out: an earlier write it overlaps may
                // be going out again, so it goes again, after that one.
                if sent.losses != self.losses {
                    self.requeue_reissue(acked);
                } else {
                    self.insert_acked(acked);
                }
            }
            (Kind::Reissue(mut acked), _) => {
                acked.attempts += 1;
                if acked.attempts > MAX_ATTEMPTS {
                    // The device will not take the only copy there is: every
                    // flush waiting is told so, and the write is gone.
                    self.released.push_back((acked.arena, acked.blocks));
                    self.fail_waiting_flushes();
                } else {
                    self.requeue_reissue(acked);
                }
            }
            // A flush that succeeded after a loss it was sent before covers
            // writes that are being issued again: it is asked again, after them.
            (Kind::Flush(ticket), Status::Ok) if sent.losses != self.losses => {
                self.queue_flush_attempt(ticket);
            }
            (Kind::Flush(ticket), Status::Ok) => {
                while self.acked.front().is_some_and(|a| a.seq < sent.covers) {
                    let a = self.acked.pop_front().expect("just seen");
                    self.released.push_back((a.arena, a.blocks));
                }
                self.attempts.remove(&ticket);
                self.answers.push_back((ticket, Outcome::Durable));
            }
            (Kind::Flush(ticket), _) => {
                self.lost();
                let attempts = self.attempts.entry(ticket).or_insert(0);
                *attempts += 1;
                if *attempts > MAX_ATTEMPTS {
                    self.attempts.remove(&ticket);
                    self.answers.push_back((ticket, Outcome::Device));
                } else {
                    self.queue_flush_attempt(ticket);
                }
            }
        }
        Ok(())
    }

    /// The session is over: the server hung up, broke the protocol, or died.
    pub fn session_ended(&mut self) {
        self.up = false;
        let wire = core::mem::take(&mut self.wire);
        let mut flushes = Vec::new();
        for sent in wire.into_values() {
            let q = sent.queued;
            match q.kind {
                Kind::User(ticket) => {
                    #[cfg(not(feature = "mutate-session-end-forgets"))]
                    {
                        self.released.push_back((q.arena, q.blocks));
                        self.answers.push_back((ticket, Outcome::Refused));
                    }
                    #[cfg(feature = "mutate-session-end-forgets")]
                    let _ = ticket;
                }
                Kind::Reissue(acked) => self.requeue_reissue(acked),
                Kind::Flush(ticket) => flushes.push(ticket),
            }
        }
        self.lost();
        for ticket in flushes {
            self.queue_flush_attempt(ticket);
        }
    }

    /// A session is open; whatever waited goes out, writes being issued
    /// again first.
    pub fn session_started(&mut self) {
        self.up = true;
    }

    /// Between [`Self::session_started`] and [`Self::session_ended`].
    pub fn up(&self) -> bool {
        self.up
    }

    /// Every answer since the last call, in the order they were decided.
    pub fn take_answers(&mut self) -> impl Iterator<Item = (Ticket, Outcome)> + '_ {
        self.answers.drain(..)
    }

    /// Arena runs `(first, blocks)` nothing will read or write again.
    pub fn take_released(&mut self) -> impl Iterator<Item = (u32, u32)> + '_ {
        self.released.drain(..)
    }

    /// Nothing is waiting, on the wire, or held for a flush.
    pub fn quiet(&self) -> bool {
        self.outbox.is_empty() && self.wire.is_empty() && self.acked.is_empty()
    }

    /// How many requests are on the wire.
    pub fn on_the_wire(&self) -> usize {
        self.wire.len()
    }

    /// How many writes have gone on the wire again after a loss.
    pub fn reissued(&self) -> u64 {
        self.reissued
    }

    fn bump(&mut self) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        seq
    }

    /// Keep `acked` in first-acknowledged order.
    fn insert_acked(&mut self, acked: Acked) {
        let at = self.acked.iter().position(|a| a.first > acked.first).unwrap_or(self.acked.len());
        self.acked.insert(at, acked);
    }

    /// Put `acked` back among the writes being issued again, in order.
    fn requeue_reissue(&mut self, acked: Acked) {
        let at = self
            .outbox
            .iter()
            .position(|q| match q.kind {
                Kind::Reissue(other) => other.first > acked.first,
                Kind::User(_) | Kind::Flush(_) => true,
            })
            .unwrap_or(self.outbox.len());
        self.outbox.insert(at, Queued {
            op: Op::Write,
            lba: acked.lba,
            blocks: acked.blocks,
            arena: acked.arena,
            kind: Kind::Reissue(acked),
        });
    }

    /// Ask the flush `ticket` again, once every write being issued again is
    /// back: after them and before anything the caller asked for since.
    fn queue_flush_attempt(&mut self, ticket: Ticket) {
        let at = self
            .outbox
            .iter()
            .position(|q| !matches!(q.kind, Kind::Reissue(_) | Kind::Flush(_)))
            .unwrap_or(self.outbox.len());
        self.outbox.insert(at, Queued { op: Op::Flush, lba: 0, blocks: 0, arena: 0, kind: Kind::Flush(ticket) });
    }

    /// The device may no longer hold any write acknowledged and not covered:
    /// each is issued again.
    fn lost(&mut self) {
        self.losses += 1;
        #[cfg(not(feature = "mutate-no-reissue-after-loss"))]
        {
            let acked = core::mem::take(&mut self.acked);
            for a in acked {
                self.requeue_reissue(a);
            }
        }
    }

    /// Every flush waiting to be asked again is answered: a write it covers
    /// is gone.
    fn fail_waiting_flushes(&mut self) {
        let mut kept = VecDeque::with_capacity(self.outbox.len());
        for q in self.outbox.drain(..) {
            match q.kind {
                Kind::Flush(ticket) => {
                    self.attempts.remove(&ticket);
                    self.answers.push_back((ticket, Outcome::Device));
                }
                Kind::User(_) | Kind::Reissue(_) => kept.push_back(q),
            }
        }
        self.outbox = kept;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(client: &mut Client, request: Request, status: Status) {
        client.complete(Completion { tag: request.tag, status }).unwrap();
    }

    #[test]
    fn nothing_goes_out_before_a_session() {
        let mut client = Client::new();
        client.submit(1, Op::Read, 0, 1, 0);
        assert_eq!(client.next_request(), None);
        client.session_started();
        assert!(client.next_request().is_some());
    }

    #[test]
    fn a_second_answer_for_one_tag_is_a_violation() {
        let mut client = Client::new();
        client.session_started();
        client.submit(1, Op::Read, 0, 1, 0);
        let r = client.next_request().unwrap();
        answer(&mut client, r, Status::Ok);
        assert_eq!(client.complete(Completion { tag: r.tag, status: Status::Ok }), Err(Violation));
    }

    /// A write acknowledged, then a flush answered `Lost`: the write goes out
    /// again before the flush is asked again, and the flush's caller hears
    /// `Durable` only from the second.
    #[test]
    fn a_lost_flush_reissues_then_asks_again() {
        let mut client = Client::new();
        client.session_started();
        client.submit(1, Op::Write, 5, 2, 3);
        let w = client.next_request().unwrap();
        answer(&mut client, w, Status::Ok);
        client.submit(2, Op::Flush, 0, 0, 0);
        let f = client.next_request().unwrap();
        answer(&mut client, f, Status::Lost);
        let again = client.next_request().unwrap();
        assert_eq!((again.op, again.lba, again.blocks, again.arena), (Op::Write, 5, 2, 3));
        assert_eq!(client.next_request(), None, "the flush waits for the write");
        answer(&mut client, again, Status::Ok);
        let f2 = client.next_request().unwrap();
        assert_eq!(f2.op, Op::Flush);
        answer(&mut client, f2, Status::Ok);
        let answers: Vec<_> = client.take_answers().collect();
        assert_eq!(answers, [(1, Outcome::Done), (2, Outcome::Durable)]);
        assert_eq!(client.take_released().collect::<Vec<_>>(), [(3, 2)]);
        assert!(client.quiet());
    }

    /// Two overlapping acknowledged writes are issued again one after the
    /// other, in the order they were first acknowledged.
    #[test]
    fn overlapping_writes_go_out_again_in_order() {
        let mut client = Client::new();
        client.session_started();
        client.submit(1, Op::Write, 0, 2, 0);
        let a = client.next_request().unwrap();
        answer(&mut client, a, Status::Ok);
        client.submit(2, Op::Write, 1, 1, 2);
        let b = client.next_request().unwrap();
        answer(&mut client, b, Status::Ok);
        client.session_ended();
        client.session_started();
        let first = client.next_request().unwrap();
        assert_eq!((first.lba, first.arena), (0, 0));
        assert_eq!(client.next_request(), None, "the overlapping one waits");
        answer(&mut client, first, Status::Ok);
        let second = client.next_request().unwrap();
        assert_eq!((second.lba, second.arena), (1, 2));
    }

    #[test]
    fn what_was_on_the_wire_at_the_end_is_refused_and_what_was_not_waits() {
        let mut client = Client::new();
        client.session_started();
        client.submit(1, Op::Write, 0, 1, 0);
        client.submit(2, Op::Read, 4, 1, 1);
        let _ = client.next_request().unwrap();
        client.session_ended();
        assert_eq!(client.take_answers().collect::<Vec<_>>(), [(1, Outcome::Refused)]);
        assert_eq!(client.take_released().collect::<Vec<_>>(), [(0, 1)]);
        client.session_started();
        let r = client.next_request().unwrap();
        assert_eq!((r.op, r.lba), (Op::Read, 4));
    }
}
