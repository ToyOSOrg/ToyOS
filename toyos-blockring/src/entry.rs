//! A request and a completion, as the words a ring carries.
//!
//! Decoding is where the other end stops being trusted: every field of a
//! request is bounded here against the arena and the partition, and a word
//! this protocol does not define is a refusal, never a default.

use crate::layout::{ARENA_BLOCKS, CQE_WORDS, MAX_REQUEST_BLOCKS, SQE_WORDS};

/// What a request asks of the partition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Op {
    /// `blocks` from the partition's block `lba` into the arena.
    Read,
    /// `blocks` from the arena to the partition's block `lba`.
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

    const fn from_word(word: u32) -> Option<Self> {
        match word {
            1 => Some(Self::Read),
            2 => Some(Self::Write),
            3 => Some(Self::Flush),
            _ => None,
        }
    }
}

/// One request. `lba` is the partition's own block number, from 0: nothing in
/// this protocol names a device block, so a neighbour's blocks have no
/// spelling. A flush carries no range, and its `lba`, `blocks` and `arena` are
/// zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Request {
    pub op: Op,
    /// The client's name for it, echoed by its completion.
    pub tag: u32,
    pub lba: u64,
    pub blocks: u32,
    /// The first arena block the data is in or goes to.
    pub arena: u32,
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
        [
            self.op.word(),
            self.tag,
            self.lba as u32,
            (self.lba >> 32) as u32,
            self.blocks,
            self.arena,
            0,
            0,
        ]
    }

    /// The request these words are, bounded against a partition of
    /// `partition_blocks`: every block it names is inside both the arena and
    /// the partition, a transfer moves at least one block and at most
    /// [`MAX_REQUEST_BLOCKS`], and a flush names nothing.
    pub fn decode(words: [u32; SQE_WORDS], partition_blocks: u64) -> Result<Self, Refused> {
        let tag = words[1];
        let refused = Refused::Malformed { tag };
        let op = Op::from_word(words[0]).ok_or(refused)?;
        let lba = u64::from(words[2]) | (u64::from(words[3]) << 32);
        let (blocks, arena) = (words[4], words[5]);
        if words[6] != 0 || words[7] != 0 {
            return Err(refused);
        }
        match op {
            Op::Flush => {
                if lba != 0 || blocks != 0 || arena != 0 {
                    return Err(refused);
                }
            }
            Op::Read | Op::Write => {
                if blocks == 0 || blocks > MAX_REQUEST_BLOCKS {
                    return Err(refused);
                }
                if arena.checked_add(blocks).is_none_or(|end| end > ARENA_BLOCKS) {
                    return Err(refused);
                }
                if lba.checked_add(u64::from(blocks)).is_none_or(|end| end > partition_blocks) {
                    return Err(refused);
                }
            }
        }
        Ok(Self { op, tag, lba, blocks, arena })
    }
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

    const fn from_word(word: u32) -> Option<Self> {
        match word {
            0 => Some(Self::Ok),
            1 => Some(Self::Invalid),
            2 => Some(Self::Device),
            3 => Some(Self::Lost),
            _ => None,
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
    pub fn decode(words: [u32; CQE_WORDS]) -> Option<Self> {
        if words[2] != 0 || words[3] != 0 {
            return None;
        }
        Some(Self { tag: words[0], status: Status::from_word(words[1])? })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PARTITION: u64 = 1000;

    #[test]
    fn a_request_survives_its_words() {
        for request in [
            Request { op: Op::Read, tag: 7, lba: 999, blocks: 1, arena: 0 },
            Request { op: Op::Write, tag: u32::MAX, lba: 0, blocks: MAX_REQUEST_BLOCKS, arena: ARENA_BLOCKS - MAX_REQUEST_BLOCKS },
            Request { op: Op::Flush, tag: 0, lba: 0, blocks: 0, arena: 0 },
        ] {
            assert_eq!(Request::decode(request.encode(), PARTITION), Ok(request));
        }
        let wide = Request { op: Op::Read, tag: 1, lba: 1 << 40, blocks: 2, arena: 3 };
        assert_eq!(Request::decode(wide.encode(), u64::MAX), Ok(wide));
    }

    /// Every field a hostile client can set out of range is refused, and the
    /// refusal still carries its tag.
    #[test]
    fn a_request_outside_its_bounds_is_refused_by_tag() {
        let good = Request { op: Op::Write, tag: 42, lba: 10, blocks: 2, arena: 5 };
        let cases: [(&str, [u32; SQE_WORDS]); 10] = [
            ("op 0", { let mut w = good.encode(); w[0] = 0; w }),
            ("op 4", { let mut w = good.encode(); w[0] = 4; w }),
            ("no blocks", Request { blocks: 0, ..good }.encode()),
            ("too many blocks", Request { blocks: MAX_REQUEST_BLOCKS + 1, ..good }.encode()),
            ("past the arena", Request { arena: ARENA_BLOCKS - 1, ..good }.encode()),
            ("arena wraps", Request { arena: u32::MAX, ..good }.encode()),
            ("past the partition", Request { lba: PARTITION - 1, ..good }.encode()),
            ("lba wraps", Request { lba: u64::MAX, ..good }.encode()),
            ("reserved word", { let mut w = good.encode(); w[7] = 1; w }),
            ("a flush with a range", Request { op: Op::Flush, ..good }.encode()),
        ];
        for (what, words) in cases {
            assert_eq!(
                Request::decode(words, PARTITION),
                Err(Refused::Malformed { tag: 42 }),
                "{what} was not refused"
            );
        }
    }

    #[test]
    fn a_completion_survives_its_words_and_refuses_what_it_does_not_define() {
        for status in [Status::Ok, Status::Invalid, Status::Device, Status::Lost] {
            let c = Completion { tag: 9, status };
            assert_eq!(Completion::decode(c.encode()), Some(c));
        }
        assert_eq!(Completion::decode([1, 4, 0, 0]), None);
        assert_eq!(Completion::decode([1, 0, 1, 0]), None);
    }
}
