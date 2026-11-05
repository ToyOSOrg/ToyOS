//! Which image the loader handed the machine to, kept on the log partition so
//! the next pass can raise the anti-rollback floor to it once that boot
//! stopped cleanly ([`crate::policy`]).
//!
//! ```text
//! partition guid [16] | booted u8 ('A' or 'B') | 0 [15] | signed-header sha256 [32]
//! ```

use crate::slots::Which;
use crate::Digest;

/// The file's length.
pub const BYTES: usize = 16 + 1 + 15 + 32;

/// The image a pass handed the machine to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Booted {
    pub slot: Which,
    /// The SHA-256 of its signed header, which names the image exactly.
    pub digest: Digest,
}

/// The file's bytes for the partition `guid` names.
pub fn encode(booted: Booted, guid: &[u8; 16]) -> [u8; BYTES] {
    let mut out = [0u8; BYTES];
    out[..16].copy_from_slice(guid);
    out[16] = booted.slot.letter() as u8;
    out[32..].copy_from_slice(&booted.digest);
    out
}

/// The image the file names, or why it is not a record for `guid`'s partition.
pub fn decode(bytes: &[u8], guid: &[u8; 16]) -> Result<Booted, Foreign> {
    let bytes: &[u8; BYTES] = bytes.try_into().map_err(|_| Foreign::Length(bytes.len()))?;
    if bytes[..16] != guid[..] {
        return Err(Foreign::Partition(bytes[..16].try_into().expect("sixteen bytes")));
    }
    let letter = bytes[16];
    let digest: Digest = bytes[32..].try_into().expect("32 bytes");
    let slot = Which::from_letter(letter as char).filter(|_| digest != [0; 32]).ok_or(Foreign::Slot(letter))?;
    Ok(Booted { slot, digest })
}

/// Why the file is not this partition's record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Foreign {
    Length(usize),
    /// It was written for another partition.
    Partition([u8; 16]),
    /// It names a booted slot that is none, or one with no image.
    Slot(u8),
}

impl core::fmt::Display for Foreign {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Length(n) => write!(f, "holds {n} bytes, wanted {BYTES}"),
            Self::Partition(g) => write!(f, "was written for {g:02x?}, another partition"),
            Self::Slot(b) => write!(f, "names slot {b:#04x}, which is no slot it booted"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GUID: [u8; 16] = [7; 16];

    fn booted(slot: Which) -> Booted {
        Booted { slot, digest: [9; 32] }
    }

    #[test]
    fn a_record_reads_back_and_anothers_is_refused() {
        for r in [booted(Which::A), booted(Which::B)] {
            assert_eq!(decode(&encode(r, &GUID), &GUID), Ok(r));
        }
        assert_eq!(decode(&encode(booted(Which::A), &[1; 16]), &GUID), Err(Foreign::Partition([1; 16])));
        assert_eq!(decode(&[0; 17], &GUID), Err(Foreign::Length(17)));
        let mut bent = encode(booted(Which::A), &GUID);
        bent[16] = b'C';
        assert_eq!(decode(&bent, &GUID), Err(Foreign::Slot(b'C')));
        let mut blank = encode(booted(Which::A), &GUID);
        blank[32..].fill(0);
        assert_eq!(decode(&blank, &GUID), Err(Foreign::Slot(b'A')), "a slot with no image");
    }
}
