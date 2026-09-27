//! A request and a completion, as the words a ring carries.
//!
//! Decoding is where the other end stops being trusted: every field of a
//! request is bounded here against the arena and the partition, and a word
//! this protocol does not define is a refusal, never a default.

use toyos_transport::{Run, Untrusted};

use crate::layout::{ARENA, CQE_WORDS, MAX_REQUEST_BLOCKS, SQE_WORDS};

/// What a request asks of the partition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Op {
    /// The run's blocks from the partition's block `lba` into the arena.
    Read,
    /// The run's blocks from the arena to the partition's block `lba`.
    Write,
    /// Every write acknowledged before this was submitted, onto the medium.
    Flush,
}

impl Op {
    const fn word(self) -> u32 {
        match self {
            Self::Read => 1,
            Self::Write => 2,
            Self::Flush => 3,
        }
    }
}

/// One request. `lba` is the partition's own block number, from 0: nothing in
/// this protocol names a device block, so a neighbour's blocks have no
/// spelling. A flush carries no range: its `lba` is zero and it names no run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Request {
    pub op: Op,
    /// The client's name for it, echoed by its completion.
    pub tag: u32,
    pub lba: u64,
    /// The arena blocks the data is in or goes to.
    pub run: Option<Run>,
}

/// Why a request was answered without being done.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Refused {
    /// A word this protocol does not define, or a range outside the arena or
    /// the partition. The tag is the request's own, so the answer still names
    /// it.
    Malformed { tag: u32 },
}

impl Request {
    pub fn encode(&self) -> [u32; SQE_WORDS] {
        let (first, count) = self.run.map_or((0, 0), |run| (run.first(), run.count()));
        [self.op.word(), self.tag, self.lba as u32, (self.lba >> 32) as u32, count, first, 0, 0]
    }

    /// The request these words are, bounded against a partition of
    /// `partition_blocks`: every block it names is inside both the arena and
    /// the partition, a transfer moves at least one block and at most
    /// [`MAX_REQUEST_BLOCKS`], and a flush names nothing.
    pub fn decode(words: [Untrusted<u32>; SQE_WORDS], partition_blocks: u64) -> Result<Self, Refused> {
        let [op, tag, lba_low, lba_high, blocks, arena, reserved @ ..] = words;
        let tag = opaque(tag);
        let refused = Refused::Malformed { tag };
        let op = [Op::Read, Op::Write, Op::Flush].into_iter().find(|o| op.is(o.word())).ok_or(refused)?;
        if !reserved.iter().all(|word| word.is(0)) {
            return Err(refused);
        }
        let lba = u64::from(opaque(lba_low)) | (u64::from(opaque(lba_high)) << 32);
        if op == Op::Flush {
            if lba != 0 || !blocks.is(0) || !arena.is(0) {
                return Err(refused);
            }
            return Ok(Self { op, tag, lba, run: None });
        }
        let run = Run::decode(arena, blocks, &ARENA).map_err(|_| refused)?;
        let blocks = run.count();
        if blocks > MAX_REQUEST_BLOCKS || lba.checked_add(u64::from(blocks)).is_none_or(|end| end > partition_blocks) {
            return Err(refused);
        }
        Ok(Self { op, tag, lba, run: Some(run) })
    }
}

/// A word every value of which means something: a tag its answer echoes, or
/// half of an `lba` the partition bounds whole.
fn opaque(word: Untrusted<u32>) -> u32 {
    word.at_most(u32::MAX.into())
        .ok()
        .and_then(|word| u32::try_from(word).ok())
        .expect("every u32 is at most u32::MAX")
}

/// What a completion says of its request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Status {
    /// Done. For a flush, every write of its writer's acknowledged before it
    /// was submitted is on the medium.
    Ok,
    /// Refused unread: the request was malformed ([`Refused`]).
    Invalid,
    /// The device did not do it, or was reset under it. A write answered this
    /// may or may not have reached the medium.
    Device,
    /// A flush that ran, and found writes of its writer's the disk lost after
    /// acknowledging them. Nothing it covers is durable until they are
    /// written again.
    Lost,
}

impl Status {
    const fn word(self) -> u32 {
        match self {
            Self::Ok => 0,
            Self::Invalid => 1,
            Self::Device => 2,
            Self::Lost => 3,
        }
    }
}

/// One completion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Completion {
    pub tag: u32,
    pub status: Status,
}

impl Completion {
    pub fn encode(&self) -> [u32; CQE_WORDS] {
        [self.tag, self.status.word(), 0, 0]
    }

    /// `None` for words no server of this protocol writes: the client treats a
    /// server that wrote them as one that broke the session.
    pub fn decode(words: [Untrusted<u32>; CQE_WORDS]) -> Option<Self> {
        let [tag, status, reserved @ ..] = words;
        if !reserved.iter().all(|word| word.is(0)) {
            return None;
        }
        let status =
            [Status::Ok, Status::Invalid, Status::Device, Status::Lost].into_iter().find(|s| status.is(s.word()))?;
        Some(Self { tag: opaque(tag), status })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PARTITION: u64 = 1000;

    fn peer<const N: usize>(words: [u32; N]) -> [Untrusted<u32>; N] {
        words.map(Untrusted::new)
    }

    #[test]
    fn a_request_survives_its_words() {
        let last = ARENA.slots() - MAX_REQUEST_BLOCKS;
        for request in [
            Request { op: Op::Read, tag: 7, lba: 999, run: ARENA.run(0, 1) },
            Request { op: Op::Write, tag: u32::MAX, lba: 0, run: ARENA.run(last, MAX_REQUEST_BLOCKS) },
            Request { op: Op::Flush, tag: 0, lba: 0, run: None },
        ] {
            assert_eq!(Request::decode(peer(request.encode()), PARTITION), Ok(request));
        }
        let wide = Request { op: Op::Read, tag: 1, lba: 1 << 40, run: ARENA.run(3, 2) };
        assert_eq!(Request::decode(peer(wide.encode()), u64::MAX), Ok(wide));
    }

    /// Every field a hostile client can set out of range is refused, and the
    /// refusal still carries its tag.
    #[test]
    fn a_request_outside_its_bounds_is_refused_by_tag() {
        let good = Request { op: Op::Write, tag: 42, lba: 10, run: ARENA.run(5, 2) };
        let with = |at: usize, word: u32| {
            let mut words = good.encode();
            words[at] = word;
            words
        };
        let cases: [(&str, [u32; SQE_WORDS]); 10] = [
            ("op 0", with(0, 0)),
            ("op 4", with(0, 4)),
            ("no blocks", with(4, 0)),
            ("too many blocks", with(4, MAX_REQUEST_BLOCKS + 1)),
            ("past the arena", with(5, ARENA.slots() - 1)),
            ("arena wraps", with(5, u32::MAX)),
            ("past the partition", Request { lba: PARTITION - 1, ..good }.encode()),
            ("lba wraps", Request { lba: u64::MAX, ..good }.encode()),
            ("reserved word", with(7, 1)),
            ("a flush with a range", Request { op: Op::Flush, ..good }.encode()),
        ];
        for (what, words) in cases {
            assert_eq!(
                Request::decode(peer(words), PARTITION),
                Err(Refused::Malformed { tag: 42 }),
                "{what} was not refused"
            );
        }
    }

    #[test]
    fn a_completion_survives_its_words_and_refuses_what_it_does_not_define() {
        for status in [Status::Ok, Status::Invalid, Status::Device, Status::Lost] {
            let c = Completion { tag: 9, status };
            assert_eq!(Completion::decode(peer(c.encode())), Some(c));
        }
        assert_eq!(Completion::decode(peer([1, 4, 0, 0])), None);
        assert_eq!(Completion::decode(peer([1, 0, 1, 0])), None);
    }
}
