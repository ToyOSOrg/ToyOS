//! What the loader keeps between its passes about the slots: which image it
//! handed the machine last, and which images died on a boot of their own.
//!
//! It extends the loader's attempt count (`bootloader/src/attempt.rs`, a file
//! on the log partition) rather than standing beside it: the count is the
//! bound on a hang, and **a panic, a fault, a wedge, a boot that never reached
//! its kernel's panic path, and a hang of an image no boot has proven are one
//! fact about a slot** — its image had the machine and gave nothing back. The
//! pass that learns it records the image's signed-header digest as dead in its
//! slot, and every later pass prefers the other slot, until an update puts a
//! different image there.
//!
//! **A hang of a proven image is not a death**: the count cannot tell a hang
//! from a power cut, and a power cut is no reason to leave an image the
//! machine has run well for an older one.
//!
//! ```text
//! partition guid [16] | count u8 | booted u8 ('A', 'B' or 0) | 0 [6]
//! | version u64 | signed-header sha256 [32] | dead A [32] | dead B [32]
//! ```
//!
//! A boot that hands the machine back on purpose proves its image, and the
//! pass that reads so raises the anti-rollback floor to its version
//! ([`crate::policy`]).

use crate::slots::Which;
use crate::Digest;

/// The file's length.
pub const BYTES: usize = 16 + 1 + 1 + 6 + 8 + 32 + 32 + 32;

/// The image a pass handed the machine to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Booted {
    pub slot: Which,
    pub version: u64,
    /// The SHA-256 of its signed header, which names the image exactly.
    pub digest: Digest,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Record {
    /// How many times this stick's images have had the machine without
    /// reporting back: the loader's bound on a hang.
    pub count: u8,
    pub booted: Option<Booted>,
    /// Per slot, the signed-header digest of an image that died there.
    pub dead: [Option<Digest>; 2],
}

/// How the last boot ended, as the loader's black box and count say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ended {
    /// It handed the machine back on purpose.
    Proven,
    /// It panicked, faulted, wedged, or never reached its own panic path.
    Died,
    /// It had the machine and the black box says nothing, twice: a hang, or a
    /// power cut, which the loader cannot tell apart.
    Hung,
    /// Nothing says: a first boot, a power cut on a machine with no black
    /// box, or a boot this record did not see.
    Unknown,
}

/// What the loader learns from how the last boot ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Accounted {
    pub record: Record,
    /// The version the last boot proved, which the floor rises to.
    pub proven: Option<u64>,
    /// The slot whose image died, now recorded dead.
    pub died: Option<Which>,
}

/// Fold how the last boot ended into the record, given the anti-rollback
/// `floor` a boot has proven: an image at or below it has run well before.
pub fn account(record: Record, ended: Ended, floor: u64) -> Accounted {
    let mut out = Accounted { record, proven: None, died: None };
    let Some(booted) = record.booted else { return out };
    let dies = match ended {
        Ended::Proven => {
            out.proven = Some(booted.version);
            false
        }
        Ended::Died => true,
        Ended::Hung => booted.version > floor,
        Ended::Unknown => false,
    };
    if dies {
        out.record.dead[booted.slot.index()] = Some(booted.digest);
        out.died = Some(booted.slot);
    }
    out
}

/// Whether the image whose signed header hashes to `digest` died in `slot`.
pub fn died(record: &Record, slot: Which, digest: &Digest) -> bool {
    record.dead[slot.index()].as_ref() == Some(digest)
}

impl Record {
    /// The file's bytes, for the partition `guid` names.
    pub fn encode(&self, guid: &[u8; 16]) -> [u8; BYTES] {
        let mut out = [0u8; BYTES];
        out[..16].copy_from_slice(guid);
        out[16] = self.count;
        if let Some(booted) = self.booted {
            out[17] = booted.slot.letter() as u8;
            out[24..32].copy_from_slice(&booted.version.to_le_bytes());
            out[32..64].copy_from_slice(&booted.digest);
        }
        for (i, dead) in self.dead.iter().enumerate() {
            if let Some(digest) = dead {
                out[64 + 32 * i..96 + 32 * i].copy_from_slice(digest);
            }
        }
        out
    }

    /// The record the file holds, or why it is not one for `guid`'s partition.
    pub fn decode(bytes: &[u8], guid: &[u8; 16]) -> Result<Self, Foreign> {
        let bytes: &[u8; BYTES] = bytes.try_into().map_err(|_| Foreign::Length(bytes.len()))?;
        if bytes[..16] != guid[..] {
            return Err(Foreign::Partition(bytes[..16].try_into().expect("sixteen bytes")));
        }
        let digest = |at: usize| -> Option<Digest> {
            let d: Digest = bytes[at..at + 32].try_into().expect("32 bytes");
            (d != [0; 32]).then_some(d)
        };
        let booted = match bytes[17] {
            0 => None,
            letter => Some(Booted {
                slot: Which::from_letter(letter as char).ok_or(Foreign::Slot(letter))?,
                version: u64::from_le_bytes(bytes[24..32].try_into().expect("eight bytes")),
                digest: digest(32).ok_or(Foreign::Slot(letter))?,
            }),
        };
        Ok(Self { count: bytes[16], booted, dead: [digest(64), digest(96)] })
    }
}

/// Why the file is not this partition's record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Foreign {
    Length(usize),
    /// It counts for another partition.
    Partition([u8; 16]),
    /// It names a booted slot that is none, or one with no image.
    Slot(u8),
}

impl core::fmt::Display for Foreign {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Length(n) => write!(f, "holds {n} bytes, wanted {BYTES}"),
            Self::Partition(g) => write!(f, "counts for {g:02x?}, another partition"),
            Self::Slot(b) => write!(f, "names slot {b:#04x}, which is no slot it booted"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GUID: [u8; 16] = [7; 16];

    fn booted(slot: Which) -> Record {
        Record { count: 1, booted: Some(Booted { slot, version: 42, digest: [9; 32] }), dead: [None, Some([3; 32])] }
    }

    #[test]
    fn a_record_reads_back_and_anothers_is_refused() {
        for r in [Record::default(), booted(Which::A), booted(Which::B)] {
            assert_eq!(Record::decode(&r.encode(&GUID), &GUID), Ok(r));
        }
        assert_eq!(Record::decode(&booted(Which::A).encode(&[1; 16]), &GUID), Err(Foreign::Partition([1; 16])));
        assert_eq!(Record::decode(&[0; 17], &GUID), Err(Foreign::Length(17)));
        let mut bent = booted(Which::A).encode(&GUID);
        bent[17] = b'C';
        assert_eq!(Record::decode(&bent, &GUID), Err(Foreign::Slot(b'C')));
    }

    /// A death marks exactly the image that died, in the slot it died in; a
    /// proven boot raises the floor and marks nothing; an unknown end does neither.
    #[test]
    fn how_a_boot_ended_is_what_it_is_recorded_as() {
        let last = booted(Which::A);
        let died = account(last, Ended::Died, 42);
        assert_eq!(died.died, Some(Which::A));
        assert!(super::died(&died.record, Which::A, &[9; 32]));
        assert!(!super::died(&died.record, Which::A, &[8; 32]), "another image in the same slot");
        assert!(super::died(&died.record, Which::B, &[3; 32]), "the other slot's record stands");
        assert_eq!(died.proven, None);

        let proven = account(last, Ended::Proven, 0);
        assert_eq!((proven.proven, proven.died, proven.record), (Some(42), None, last));
        let unknown = account(last, Ended::Unknown, 0);
        assert_eq!((unknown.proven, unknown.died, unknown.record), (None, None, last));
        assert_eq!(account(Record::default(), Ended::Died, 0).died, None, "nothing was booted");
    }

    /// A hang is a death only for an image no boot has proven: at or below the
    /// floor it is a power cut as far as anything here can tell.
    #[test]
    fn a_hang_kills_only_an_unproven_image() {
        let last = booted(Which::B);
        assert_eq!(account(last, Ended::Hung, 41).died, Some(Which::B), "version 42 over a floor of 41");
        assert_eq!(account(last, Ended::Hung, 42).died, None, "version 42 at a floor of 42");
        assert_eq!(account(last, Ended::Died, 100).died, Some(Which::B), "a panic is a death whatever the floor");
    }
}
