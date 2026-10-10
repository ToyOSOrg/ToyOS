//! What a block service decides about one session's requests: which are in
//! flight, what each completion says, and whose flush answers for which
//! writes the disk lost.
//!
//! **Every request taken is answered exactly once.** [`ServerSession::take`]
//! records a request as in flight and [`ServerSession::complete`] or
//! [`ServerSession::abort_all`] is the one place it leaves; a device answer for
//! a request no longer in flight — one a reset already answered — is dropped
//! rather than answered twice.
//!
//! **A flush answers for its writer.** A session is one writer over one span
//! of the device ([`toyos_blockhold`]); a write's completion records it under
//! the device's loss count, and a flush's completion under the same count
//! settles every writer and fails for this one if the disk lost writes of its
//! own since it last heard. The loss count is the device's, bumped by whoever
//! resets it.

use alloc::vec::Vec;

use toyos_blockhold::{Holds, Writer};
use toyos_transport::Untrusted;

use crate::entry::{Completion, Op, Refused, Request, Status};
use crate::layout::SQE_WORDS;

/// One session's side of the bookkeeping; the device's [`Holds`] is passed
/// to each call that needs it, because it is every session's.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ServerSession {
    /// The partition's length, which every request is bounded by.
    blocks: u64,
    /// The span of the device this session holds, which is its writer.
    first: u64,
    /// Its grant lets it write; a write on a session whose grant does not is
    /// answered `Invalid` and never reaches the device.
    writes: bool,
    /// Each request in flight, in the order it was taken: a tag is only ever
    /// compared for equality.
    inflight: Vec<(u32, Op)>,
}

/// A request the device is to be asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Taken {
    /// Issue it.
    Issue(Request),
    /// Answer it at once, unissued.
    Answer(Completion),
}

impl ServerSession {
    /// A session over the partition at device block `first`, `blocks` long,
    /// which `holds` already holds for it, taking writes if `writes`.
    pub fn new(first: u64, blocks: u64, writes: bool) -> Self {
        Self { blocks, first, writes, inflight: Vec::new() }
    }

    /// The writer this session's writes and flushes are accounted to.
    pub fn writer(&self) -> Writer {
        Writer::Span(self.first)
    }

    /// Where the partition starts on the device, which a request's `lba` is
    /// added to.
    pub fn first(&self) -> u64 {
        self.first
    }

    pub fn blocks(&self) -> u64 {
        self.blocks
    }

    /// Requests taken and not yet answered, in the order they were taken.
    pub fn inflight(&self) -> impl ExactSizeIterator<Item = (u32, Op)> + '_ {
        self.inflight.iter().copied()
    }

    /// Decide what one entry the client published is.
    ///
    /// A malformed entry, a write its grant does not let it make, and a tag
    /// already in flight, are answered at once and never reach the device:
    /// the last would make one tag two requests, and the client could not
    /// tell which answer was whose.
    pub fn take(&mut self, words: [Untrusted<u32>; SQE_WORDS]) -> Taken {
        let request = match Request::decode(words, self.blocks) {
            Ok(request) => request,
            Err(Refused::Malformed { tag }) => {
                return Taken::Answer(Completion { tag, status: Status::Invalid })
            }
        };
        if !self.writes && matches!(request.op, Op::Write { .. }) {
            return Taken::Answer(Completion { tag: request.tag, status: Status::Invalid });
        }
        if self.inflight.iter().any(|&(tag, _)| tag == request.tag) {
            return Taken::Answer(Completion { tag: request.tag, status: Status::Invalid });
        }
        self.inflight.push((request.tag, request.op));
        Taken::Issue(request)
    }

    /// The device answered the request `tag` with `done` (whether it did it),
    /// while its loss count was `losses`. `None` for a tag no longer in
    /// flight: a reset has already answered it.
    pub fn complete<H: Copy>(
        &mut self,
        tag: u32,
        done: bool,
        holds: &mut Holds<H>,
        losses: u64,
    ) -> Option<Completion> {
        let at = self.inflight.iter().position(|&(t, _)| t == tag)?;
        let (_, op) = self.inflight.remove(at);
        let status = match (op, done) {
            (_, false) => Status::Device,
            (Op::Read { .. }, true) => Status::Ok,
            (Op::Write { .. }, true) => {
                holds.wrote(self.writer(), losses);
                Status::Ok
            }
            (Op::Flush, true) => match holds.flushed(self.writer(), losses) {
                Ok(()) => Status::Ok,
                Err(_) => Status::Lost,
            },
        };
        Some(Completion { tag, status })
    }

    /// The device was reset under every request in flight: each is answered
    /// [`Status::Device`], in the order it was taken, and a late answer from
    /// the device for any of them finds nothing to complete.
    pub fn abort_all(&mut self) -> Vec<Completion> {
        let answered = self.inflight.iter().map(|&(tag, _)| Completion { tag, status: Status::Device }).collect();
        #[cfg(not(feature = "mutate-abort-keeps-inflight"))]
        self.inflight.clear();
        answered
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::ARENA;

    fn write(tag: u32) -> [Untrusted<u32>; SQE_WORDS] {
        Request { op: Op::Write { run: ARENA.run(0, 1).unwrap(), lba: 0 }, tag }.encode().map(Untrusted::new)
    }
    fn flush(tag: u32) -> [Untrusted<u32>; SQE_WORDS] {
        Request { op: Op::Flush, tag }.encode().map(Untrusted::new)
    }

    #[test]
    fn a_reset_answers_once_and_the_late_device_answer_is_dropped() {
        let mut holds: Holds<u8> = Holds::new();
        holds.hold(10, 20, 1).unwrap();
        let mut session = ServerSession::new(10, 10, true);
        assert!(matches!(session.take(write(1)), Taken::Issue(_)));
        assert_eq!(session.abort_all(), [Completion { tag: 1, status: Status::Device }]);
        assert_eq!(session.complete(1, true, &mut holds, 1), None);
    }

    /// A reset answers in the order the requests were taken, whatever their
    /// tags' values: nothing orders a tag but its arrival.
    #[test]
    fn a_reset_answers_in_the_order_taken() {
        let mut session = ServerSession::new(0, 10, true);
        for tag in [9, 4] {
            assert!(matches!(session.take(write(tag)), Taken::Issue(_)));
        }
        let answered: Vec<u32> = session.abort_all().iter().map(|c| c.tag).collect();
        assert_eq!(answered, [9, 4]);
    }

    #[test]
    fn a_tag_in_flight_twice_is_refused_unissued() {
        let mut session = ServerSession::new(0, 10, true);
        assert!(matches!(session.take(write(4)), Taken::Issue(_)));
        assert_eq!(session.take(write(4)), Taken::Answer(Completion { tag: 4, status: Status::Invalid }));
        assert_eq!(session.inflight().len(), 1);
    }

    /// A write acknowledged before a reset bumped the loss count is a loss the
    /// session's next flush reports — once.
    #[test]
    fn a_flush_after_a_loss_answers_lost_once() {
        let mut holds: Holds<u8> = Holds::new();
        holds.hold(0, 10, 1).unwrap();
        let mut session = ServerSession::new(0, 10, true);
        session.take(write(1));
        assert_eq!(session.complete(1, true, &mut holds, 0).unwrap().status, Status::Ok);
        session.take(flush(2));
        assert_eq!(session.complete(2, true, &mut holds, 1).unwrap().status, Status::Lost);
        session.take(flush(3));
        assert_eq!(session.complete(3, true, &mut holds, 1).unwrap().status, Status::Ok);
    }

    /// A session whose grant does not write answers a write `Invalid` and
    /// holds nothing for the device, and still reads and flushes.
    #[test]
    fn a_read_only_session_refuses_a_write_unissued() {
        let mut session = ServerSession::new(0, 10, false);
        assert_eq!(session.take(write(1)), Taken::Answer(Completion { tag: 1, status: Status::Invalid }));
        assert_eq!(session.inflight().len(), 0);
        let read = Request { op: Op::Read { run: ARENA.run(0, 1).unwrap(), lba: 0 }, tag: 2 };
        assert!(matches!(session.take(read.encode().map(Untrusted::new)), Taken::Issue(_)));
        assert!(matches!(session.take(flush(3)), Taken::Issue(_)));
    }
}
