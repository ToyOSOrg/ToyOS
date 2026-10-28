//! What a client decides about its requests: which go on the wire and when,
//! what each completion means to whoever asked, and what has to be written
//! again after a loss.
//!
//! **Every request asked for is answered exactly once**, by [`Client::complete`]
//! or by [`Client::session_ended`], and never twice: a completion for a tag
//! that is not on the wire is the server breaking the session
//! ([`Violation::Tag`]), not a second answer.
//!
//! **The arena holds only what is on the wire.** A read or a write is given
//! its arena blocks as it goes out ([`Client::next_request`]'s `place`) and
//! gives them back when it is answered ([`Client::take_released`]); nothing
//! waiting to go out, and nothing acknowledged, holds any.
//!
//! **An acknowledged write is kept until a flush covers it**, by its ticket:
//! the caller keeps its bytes until [`Client::take_retired`] names it, because
//! a loss — a flush answered [`Status::Lost`] or [`Status::Device`], or a
//! session that ended — means the device may no longer hold it, and the only
//! copy left is the caller's. After a loss every such write is issued again, in the order
//! it was first acknowledged and never beside an earlier one it overlaps, and
//! nothing else goes on the wire until they are all acknowledged again; the
//! flush that was waiting is then asked again. A flush is answered
//! [`Outcome::Durable`] only by a device flush that succeeded after every loss
//! it could have been told about.
//!
//! **A write given up is a loss no later flush can answer for.** One the device
//! refused on more than [`MAX_ATTEMPTS`] reissues is dropped, and its caller
//! was told [`Outcome::Done`] for it: every flush from then on, waiting or
//! asked later, covers it and is answered [`Outcome::Device`], for the
//! client's life.
//!
//! **What was on the wire when a session ended is answered
//! [`Outcome::Refused`]**: a write it took may or may not be on the disk, and
//! the caller is told so rather than told either. What had not yet been put on
//! the wire waits for the next session, unchanged.
//!
//! **Requests in flight at once are unordered.** A caller must not have two in
//! flight whose ranges overlap when either is a write, nor reuse arena blocks
//! before they are released.
//!
//! **A write's bytes are the caller's to keep** from [`Client::submit`] until
//! the write is answered anything but [`Outcome::Done`], or until
//! [`Client::take_retired`] names its ticket; `place` copies them into the
//! arena each time the write goes out.

use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;

use toyos::transport::{Inflight, Run, Violation};

use crate::entry::{Completion, Op, Request, Status};

/// What a caller asks for. A transfer names its blocks, not the arena blocks
/// it will move through: those are given as it goes on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Ask {
    Read { lba: u64, blocks: u32 },
    Write { lba: u64, blocks: u32 },
    Flush,
}

/// A transfer going on the wire, for `place` to give arena blocks: a write's
/// are filled with the bytes the caller keeps for `ticket`, the write's own,
/// when it is the caller's or issued again.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Placing {
    pub ticket: Ticket,
    pub write: bool,
    pub blocks: u32,
}
use crate::layout::DEPTH;

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
    /// A write the session's grant does not make, refused unissued.
    ReadOnly,
    /// The device did not do it. A write answered this may or may not have
    /// reached the medium; a flush answered this left writes the device would
    /// not keep.
    Device,
    /// The session ended with it on the wire: it may or may not have happened.
    Refused,
}

/// A write acknowledged and not yet covered by a flush.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Acked {
    lba: u64,
    blocks: u32,
    /// The caller's ticket for it, by which the caller keeps its bytes.
    ticket: Ticket,
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
    ask: Ask,
    kind: Kind,
}

impl Queued {
    /// The ticket whose bytes a write carries.
    fn ticket(&self) -> Ticket {
        match self.kind {
            Kind::User(ticket) | Kind::Flush(ticket) => ticket,
            Kind::Reissue(acked) => acked.ticket,
        }
    }
}

/// A request on the wire, the op it went as, and for a flush the
/// acknowledgement it covers below and the losses it was sent after.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Sent {
    queued: Queued,
    op: Op,
    covers: u64,
    losses: u64,
}

impl Sent {
    fn run(&self) -> Option<Run> {
        match self.op {
            Op::Read { run, .. } | Op::Write { run, .. } => Some(run),
            Op::Flush => None,
        }
    }
}

/// `D` is the most requests it has on the wire at once: a session's [`DEPTH`].
#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct Client<const D: usize = { DEPTH as usize }> {
    up: bool,
    /// Not yet on the wire, in order. Writes being issued again are always at
    /// the front, then any flush attempt waiting on them.
    outbox: VecDeque<Queued>,
    wire: Inflight<Sent, D>,
    acked: VecDeque<Acked>,
    next_seq: u64,
    /// Losses taken so far: a flush sent before one says nothing about what it
    /// lost.
    losses: u64,
    attempts: BTreeMap<Ticket, u32>,
    answers: VecDeque<(Ticket, Outcome)>,
    released: VecDeque<Run>,
    /// Write tickets whose bytes nothing will send again.
    retired: VecDeque<Ticket>,
    /// Writes put on the wire again after a loss, over the client's life.
    reissued: u64,
    /// An acknowledged write was given up: no flush is durable again.
    gave_up: bool,
}

impl<const D: usize> Client<D> {
    /// A client with no session yet: requests wait until
    /// [`Self::session_started`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask for a read, a write or a flush.
    pub fn submit(&mut self, ticket: Ticket, ask: Ask) {
        let kind = match ask {
            Ask::Flush => Kind::Flush(ticket),
            Ask::Read { .. } | Ask::Write { .. } => Kind::User(ticket),
        };
        self.outbox.push_back(Queued { ask, kind });
    }

    fn reissuing(&self) -> bool {
        self.wire.values().any(|sent| matches!(sent.queued.kind, Kind::Reissue(_)))
            || self.outbox.front().is_some_and(|q| matches!(q.kind, Kind::Reissue(_)))
    }

    /// The next request to put on the wire, tagged, its transfer placed in the
    /// arena by `place`; `None` while there is no session, nothing to send,
    /// `D` on the wire, what is next must wait for writes being issued again,
    /// or `place` has no room for it, which a request on the wire answered
    /// makes.
    pub fn next_request(&mut self, mut place: impl FnMut(Placing) -> Option<Run>) -> Option<Request> {
        if !self.up || self.wire.values().count() == D {
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
                    matches!(sent.op, Op::Write { run, lba } if acked.overlaps_range(lba, run.count()))
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
        let op = match front.ask {
            Ask::Flush => Op::Flush,
            Ask::Read { lba, blocks } => {
                Op::Read { run: place(Placing { ticket: front.ticket(), write: false, blocks })?, lba }
            }
            Ask::Write { lba, blocks } => {
                Op::Write { run: place(Placing { ticket: front.ticket(), write: true, blocks })?, lba }
            }
        };
        let sent = Sent { queued: front, op, covers: self.next_seq, losses: self.losses };
        let tag = self.wire.insert(sent).expect("blockring: a slot counted free was taken");
        self.outbox.pop_front();
        if matches!(front.kind, Kind::Reissue(_)) {
            self.reissued += 1;
        }
        Some(Request { op, tag })
    }

    /// What the server answered.
    pub fn complete(&mut self, completion: Completion) -> Result<(), Violation> {
        let sent = self.wire.answer(completion.tag)?;
        let q = sent.queued;
        // Answered, whatever the word: its arena blocks are free once a read's
        // data has been taken, which the caller does before it releases them.
        self.released.extend(sent.run());
        match (q.kind, completion.status) {
            (Kind::User(ticket), Status::Ok) => {
                match sent.op {
                    Op::Write { run, lba } => {
                        let seq = self.bump();
                        let acked = Acked { lba, blocks: run.count(), ticket, first: seq, seq, attempts: 0 };
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
                    Op::Read { .. } => {}
                    Op::Flush => return Err(Violation::Entry),
                }
                self.answers.push_back((ticket, Outcome::Done));
            }
            (Kind::User(ticket), status) => {
                let outcome = match status {
                    Status::Invalid => Outcome::Invalid,
                    Status::Device => Outcome::Device,
                    Status::ReadOnly if matches!(sent.op, Op::Write { .. }) => Outcome::ReadOnly,
                    // A read or a write answered `Lost`, or a read answered
                    // `ReadOnly`, is a server that does not know which op it
                    // was.
                    Status::ReadOnly | Status::Lost | Status::Ok => return Err(Violation::Entry),
                };
                self.answers.push_back((ticket, outcome));
            }
            // A write the session took once, refused by the same grant on its
            // way again, or a flush refused as a write: a server that does
            // not know what it granted.
            (Kind::Reissue(_) | Kind::Flush(_), Status::ReadOnly) => return Err(Violation::Entry),
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
                    self.retired.push_back(acked.ticket);
                    self.gave_up = true;
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
                    self.retired.push_back(a.ticket);
                }
                self.attempts.remove(&ticket);
                let outcome = if self.gave_up { Outcome::Device } else { Outcome::Durable };
                self.answers.push_back((ticket, outcome));
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
        let mut wire = Vec::new();
        self.wire.end(|_, sent| wire.push(sent));
        let mut flushes = Vec::new();
        for sent in wire {
            let q = sent.queued;
            self.released.extend(sent.run());
            match q.kind {
                Kind::User(ticket) => {
                    #[cfg(not(feature = "mutate-session-end-forgets"))]
                    self.answers.push_back((ticket, Outcome::Refused));
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

    /// Arena runs nothing will read or write again.
    pub fn take_released(&mut self) -> impl Iterator<Item = Run> + '_ {
        self.released.drain(..)
    }

    /// Acknowledged writes, by ticket, that nothing will send again: a flush
    /// covered them, or they were given up.
    pub fn take_retired(&mut self) -> impl Iterator<Item = Ticket> + '_ {
        self.retired.drain(..)
    }

    /// Nothing is waiting, on the wire, or held for a flush.
    pub fn quiet(&self) -> bool {
        self.outbox.is_empty() && self.wire.values().next().is_none() && self.acked.is_empty()
    }

    /// The tags on the wire.
    pub fn on_the_wire(&self) -> impl Iterator<Item = u32> + '_ {
        self.wire.tags()
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
        self.outbox.insert(at, Queued { ask: Ask::Write { lba: acked.lba, blocks: acked.blocks }, kind: Kind::Reissue(acked) });
    }

    /// Ask the flush `ticket` again, once every write being issued again is
    /// back: after them and before anything the caller asked for since.
    fn queue_flush_attempt(&mut self, ticket: Ticket) {
        let at = self
            .outbox
            .iter()
            .position(|q| !matches!(q.kind, Kind::Reissue(_) | Kind::Flush(_)))
            .unwrap_or(self.outbox.len());
        self.outbox.insert(at, Queued { ask: Ask::Flush, kind: Kind::Flush(ticket) });
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
    use crate::layout::ARENA;

    /// The arena blocks every test gives a transfer: four from its ticket's
    /// fourth, so each ticket's are its own and a test can name them.
    fn run_of(ticket: Ticket, blocks: u32) -> Run {
        ARENA.run(ticket as u32 * 4, blocks).unwrap()
    }

    fn place(p: Placing) -> Option<Run> {
        Some(run_of(p.ticket, p.blocks))
    }

    fn next(client: &mut Client) -> Option<Request> {
        client.next_request(place)
    }

    fn answer(client: &mut Client, request: Request, status: Status) {
        client.complete(Completion { tag: request.tag, status }).unwrap();
    }

    #[test]
    fn nothing_goes_out_before_a_session() {
        let mut client: Client = Client::new();
        client.submit(1, Ask::Read { lba: 0, blocks: 1 });
        assert_eq!(next(&mut client), None);
        client.session_started();
        assert!(next(&mut client).is_some());
    }

    #[test]
    fn a_second_answer_for_one_tag_is_a_violation() {
        let mut client: Client = Client::new();
        client.session_started();
        client.submit(1, Ask::Read { lba: 0, blocks: 1 });
        let r = next(&mut client).unwrap();
        answer(&mut client, r, Status::Ok);
        assert_eq!(client.complete(Completion { tag: r.tag, status: Status::Ok }), Err(Violation::Tag));
    }

    /// An acknowledged write gives its arena blocks back at once, and its
    /// ticket is retired only by the flush that covers it.
    #[test]
    fn a_write_holds_arena_blocks_only_on_the_wire() {
        let mut client: Client = Client::new();
        client.session_started();
        client.submit(1, Ask::Write { lba: 5, blocks: 2 });
        let w = next(&mut client).unwrap();
        assert_eq!(w.op, Op::Write { run: run_of(1, 2), lba: 5 });
        answer(&mut client, w, Status::Ok);
        assert_eq!(client.take_released().collect::<Vec<_>>(), [run_of(1, 2)]);
        assert_eq!(client.take_retired().count(), 0, "acknowledged, not yet covered");
        client.submit(2, Ask::Flush);
        let f = next(&mut client).unwrap();
        answer(&mut client, f, Status::Ok);
        assert_eq!(client.take_retired().collect::<Vec<_>>(), [1]);
        assert!(client.quiet());
    }

    /// A request `place` has no room for waits, unsent and unanswered, and
    /// goes once there is room.
    #[test]
    fn a_request_without_arena_room_waits_for_it() {
        let mut client: Client = Client::new();
        client.session_started();
        client.submit(1, Ask::Write { lba: 0, blocks: 1 });
        assert_eq!(client.next_request(|_| None), None);
        assert_eq!(client.take_answers().count(), 0);
        let w = next(&mut client).unwrap();
        assert_eq!(w.op, Op::Write { run: run_of(1, 1), lba: 0 });
    }

    /// A write acknowledged, then a flush answered `Lost`: the write goes out
    /// again, placed again under its own ticket, before the flush is asked
    /// again, and the flush's caller hears `Durable` only from the second.
    #[test]
    fn a_lost_flush_reissues_then_asks_again() {
        let mut client: Client = Client::new();
        client.session_started();
        client.submit(1, Ask::Write { lba: 5, blocks: 2 });
        let w = next(&mut client).unwrap();
        answer(&mut client, w, Status::Ok);
        client.submit(2, Ask::Flush);
        let f = next(&mut client).unwrap();
        answer(&mut client, f, Status::Lost);
        let mut placed = Vec::new();
        let again = client.next_request(|p| { placed.push(p); place(p) }).unwrap();
        assert_eq!(placed, [Placing { ticket: 1, write: true, blocks: 2 }]);
        assert_eq!(again.op, Op::Write { run: run_of(1, 2), lba: 5 });
        assert_eq!(next(&mut client), None, "the flush waits for the write");
        answer(&mut client, again, Status::Ok);
        let f2 = next(&mut client).unwrap();
        assert_eq!(f2.op, Op::Flush);
        answer(&mut client, f2, Status::Ok);
        let answers: Vec<_> = client.take_answers().collect();
        assert_eq!(answers, [(1, Outcome::Done), (2, Outcome::Durable)]);
        assert_eq!(client.take_released().collect::<Vec<_>>(), [run_of(1, 2), run_of(1, 2)]);
        assert_eq!(client.take_retired().collect::<Vec<_>>(), [1]);
        assert!(client.quiet());
    }

    /// Two overlapping acknowledged writes are issued again one after the
    /// other, in the order they were first acknowledged.
    #[test]
    fn overlapping_writes_go_out_again_in_order() {
        let mut client: Client = Client::new();
        client.session_started();
        client.submit(1, Ask::Write { lba: 0, blocks: 2 });
        let a = next(&mut client).unwrap();
        answer(&mut client, a, Status::Ok);
        client.submit(2, Ask::Write { lba: 1, blocks: 1 });
        let b = next(&mut client).unwrap();
        answer(&mut client, b, Status::Ok);
        client.session_ended();
        client.session_started();
        let first = next(&mut client).unwrap();
        assert_eq!(first.op, Op::Write { run: run_of(1, 2), lba: 0 });
        assert_eq!(next(&mut client), None, "the overlapping one waits");
        answer(&mut client, first, Status::Ok);
        let second = next(&mut client).unwrap();
        assert_eq!(second.op, Op::Write { run: run_of(2, 1), lba: 1 });
    }

    /// A write the device refused on every reissue is gone, and its caller was
    /// told it was done: its ticket is retired, and a flush asked for after
    /// that is answered the loss, never durable.
    #[test]
    fn a_write_given_up_poisons_every_later_flush() {
        let mut client: Client = Client::new();
        client.session_started();
        client.submit(1, Ask::Write { lba: 0, blocks: 1 });
        let w = next(&mut client).unwrap();
        answer(&mut client, w, Status::Ok);
        client.session_ended();
        for _ in 0..=MAX_ATTEMPTS {
            client.session_started();
            let again = next(&mut client).unwrap();
            assert_eq!(again.op, Op::Write { run: run_of(1, 1), lba: 0 });
            answer(&mut client, again, Status::Device);
        }
        assert_eq!(client.take_retired().collect::<Vec<_>>(), [1]);
        client.submit(2, Ask::Flush);
        let f = next(&mut client).unwrap();
        assert_eq!(f.op, Op::Flush);
        answer(&mut client, f, Status::Ok);
        assert_eq!(client.take_answers().collect::<Vec<_>>(), [(1, Outcome::Done), (2, Outcome::Device)]);
        client.submit(3, Ask::Flush);
        let f = next(&mut client).unwrap();
        answer(&mut client, f, Status::Ok);
        assert_eq!(client.take_answers().collect::<Vec<_>>(), [(3, Outcome::Device)]);
    }

    /// A write refused by its grant is the caller's `ReadOnly` and gives its
    /// arena blocks back; the word on a read or a flush is a server that does
    /// not know which op it answered.
    #[test]
    fn read_only_answers_a_write_alone() {
        let mut client: Client = Client::new();
        client.session_started();
        client.submit(1, Ask::Write { lba: 0, blocks: 1 });
        let w = next(&mut client).unwrap();
        answer(&mut client, w, Status::ReadOnly);
        assert_eq!(client.take_answers().collect::<Vec<_>>(), [(1, Outcome::ReadOnly)]);
        assert_eq!(client.take_released().collect::<Vec<_>>(), [run_of(1, 1)]);
        for ask in [Ask::Read { lba: 0, blocks: 1 }, Ask::Flush] {
            client.submit(2, ask);
            let r = next(&mut client).unwrap();
            assert_eq!(client.complete(Completion { tag: r.tag, status: Status::ReadOnly }), Err(Violation::Entry));
        }
    }

    #[test]
    fn what_was_on_the_wire_at_the_end_is_refused_and_what_was_not_waits() {
        let mut client: Client = Client::new();
        client.session_started();
        client.submit(1, Ask::Write { lba: 0, blocks: 1 });
        client.submit(2, Ask::Read { lba: 4, blocks: 1 });
        let _ = client.next_request(place).unwrap();
        client.session_ended();
        assert_eq!(client.take_answers().collect::<Vec<_>>(), [(1, Outcome::Refused)]);
        assert_eq!(client.take_released().collect::<Vec<_>>(), [run_of(1, 1)]);
        client.session_started();
        let r = next(&mut client).unwrap();
        assert_eq!(r.op, Op::Read { run: run_of(2, 1), lba: 4 });
    }
}
