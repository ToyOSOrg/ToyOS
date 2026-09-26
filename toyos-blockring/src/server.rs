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

use alloc::collections::BTreeMap;

use toyos_blockhold::{Holds, Writer};

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
    /// By tag: the op each request in flight asked for.
    inflight: BTreeMap<u32, Op>,
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
    /// which `holds` already holds for it.
    pub fn new(first: u64, blocks: u64) -> Self {
        Self { blocks, first, inflight: BTreeMap::new() }
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

    /// Requests taken and not yet answered.
    pub fn inflight(&self) -> usize {
        self.inflight.len()
    }

    /// Decide what one entry the client published is.
    ///
    /// A malformed entry, and a tag already in flight, are answered at once
    /// and never reach the device: the second would make one tag two
    /// requests, and the client could not tell which answer was whose.
    pub fn take(&mut self, words: [u32; SQE_WORDS]) -> Taken {
        let request = match Request::decode(words, self.blocks) {
            Ok(request) => request,
            Err(Refused::Malformed { tag }) => {
                return Taken::Answer(Completion { tag, status: Status::Invalid })
            }
        };
        if self.inflight.contains_key(&request.tag) {
            return Taken::Answer(Completion { tag: request.tag, status: Status::Invalid });
        }
        self.inflight.insert(request.tag, request.op);
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
        let op = self.inflight.remove(&tag)?;
        let status = match (op, done) {
            (_, false) => Status::Device,
            (Op::Read, true) => Status::Ok,
            (Op::Write, true) => {
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
    /// [`Status::Device`], and a late answer from the device for any of them
    /// finds nothing to complete.
    pub fn abort_all(&mut self) -> alloc::vec::Vec<Completion> {
        let answered = self
            .inflight
            .keys()
            .map(|&tag| Completion { tag, status: Status::Device })
            .collect();
        #[cfg(not(feature = "mutate-abort-keeps-inflight"))]
        self.inflight.clear();
        answered
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(tag: u32) -> [u32; SQE_WORDS] {
        Request { op: Op::Write, tag, lba: 0, blocks: 1, arena: 0 }.encode()
    }
    fn flush(tag: u32) -> [u32; SQE_WORDS] {
        Request { op: Op::Flush, tag, lba: 0, blocks: 0, arena: 0 }.encode()
    }

    #[test]
    fn a_reset_answers_once_and_the_late_device_answer_is_dropped() {
        let mut holds: Holds<u8> = Holds::new();
        holds.hold(10, 20, 1).unwrap();
        let mut session = ServerSession::new(10, 10);
        assert!(matches!(session.take(write(1)), Taken::Issue(_)));
        assert_eq!(session.abort_all(), [Completion { tag: 1, status: Status::Device }]);
        assert_eq!(session.complete(1, true, &mut holds, 1), None);
    }

    #[test]
    fn a_tag_in_flight_twice_is_refused_unissued() {
        let mut session = ServerSession::new(0, 10);
        assert!(matches!(session.take(write(4)), Taken::Issue(_)));
        assert_eq!(session.take(write(4)), Taken::Answer(Completion { tag: 4, status: Status::Invalid }));
        assert_eq!(session.inflight(), 1);
    }

    /// A write acknowledged before a reset bumped the loss count is a loss the
    /// session's next flush reports — once.
    #[test]
    fn a_flush_after_a_loss_answers_lost_once() {
        let mut holds: Holds<u8> = Holds::new();
        holds.hold(0, 10, 1).unwrap();
        let mut session = ServerSession::new(0, 10);
        session.take(write(1));
        assert_eq!(session.complete(1, true, &mut holds, 0).unwrap().status, Status::Ok);
        session.take(flush(2));
        assert_eq!(session.complete(2, true, &mut holds, 1).unwrap().status, Status::Lost);
        session.take(flush(3));
        assert_eq!(session.complete(3, true, &mut holds, 1).unwrap().status, Status::Ok);
    }
}
