//! The IGD OpRegion, read as input from outside, by Intel's "IGD
//! OpRegion/Software SCI/_DSM for Skylake Processors BIOS Specification",
//! revision 0.5: its header (§3.1) and the mailboxes it says it carries.
//!
//! The kernel already held the region to its signature and 8 KB; this is
//! read from a copy, so every field is validated here again, and the raw VBT
//! is handed out unparsed.

/// §3.1.1.
const SIGNATURE: &[u8; 16] = b"IntelGraphicsMem";
/// §3.1.2: "OpRegion Size = (incl. Header) in KB = 8 KB".
pub const BYTES: usize = 8 * 1024;
const SIZE_AT: usize = 0x10;
const OVER_AT: usize = 0x14;
const MBOX_AT: usize = 0x58;
/// Table 3-1: mailbox 4, the VBT, is 6 KB at 400h.
const VBT_AT: usize = 0x400;
const VBT_BYTES: usize = 6 * 1024;
/// §3.1.7 MBOX: bit 3 mailbox 4, the VBT, which "Must be supported". The
/// bits the 2016 revision calls reserved may name mailboxes a later one
/// defines, so they are not refused.
const MBOX_VBT: u32 = 1 << 3;

/// Why a copy of the OpRegion was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Malformed {
    /// Not the 8 KB §3.1.2 gives.
    Length(usize),
    Signature,
    Size(u32),
    /// MBOX does not name the VBT's mailbox.
    Mailboxes(u32),
}

/// A validated copy of the OpRegion.
#[derive(Debug)]
pub struct OpRegion<'a> {
    bytes: &'a [u8],
    /// OVER: the OpRegion/SWSCI interface version firmware implements.
    pub version: u32,
    /// MBOX: one bit per mailbox it carries.
    pub mailboxes: u32,
}

impl<'a> OpRegion<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Malformed> {
        if bytes.len() != BYTES {
            return Err(Malformed::Length(bytes.len()));
        }
        let word = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().expect("four bytes"));
        if &bytes[..16] != SIGNATURE {
            return Err(Malformed::Signature);
        }
        if word(SIZE_AT) != (BYTES / 1024) as u32 {
            return Err(Malformed::Size(word(SIZE_AT)));
        }
        let mailboxes = word(MBOX_AT);
        if mailboxes & MBOX_VBT == 0 {
            return Err(Malformed::Mailboxes(mailboxes));
        }
        Ok(Self { bytes, version: word(OVER_AT), mailboxes })
    }

    /// Mailbox 4's raw VBT, 6 KB of it, unparsed.
    pub fn vbt(&self) -> &'a [u8] {
        &self.bytes[VBT_AT..VBT_AT + VBT_BYTES]
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::vec::Vec;

    fn region(mailboxes: u32) -> Vec<u8> {
        let mut bytes = std::vec![0u8; BYTES];
        bytes[..16].copy_from_slice(SIGNATURE);
        bytes[SIZE_AT..SIZE_AT + 4].copy_from_slice(&8u32.to_le_bytes());
        bytes[OVER_AT..OVER_AT + 4].copy_from_slice(&0x0200_0000u32.to_le_bytes());
        bytes[MBOX_AT..MBOX_AT + 4].copy_from_slice(&mailboxes.to_le_bytes());
        bytes[VBT_AT] = 0x24;
        bytes
    }

    #[test]
    fn a_region_firmware_set_up_gives_its_version_mailboxes_and_vbt() {
        let bytes = region(0b1_1111);
        let opregion = OpRegion::parse(&bytes).expect("parses");
        assert_eq!((opregion.version, opregion.mailboxes), (0x0200_0000, 0b1_1111));
        assert_eq!((opregion.vbt().len(), opregion.vbt()[0]), (6 * 1024, 0x24));
    }

    #[test]
    fn a_copy_that_does_not_hold_together_is_refused() {
        assert_eq!(OpRegion::parse(&region(0b1111)[..BYTES - 1]).err(), Some(Malformed::Length(BYTES - 1)));
        let mut bad = region(0b1111);
        bad[15] = b'M';
        assert_eq!(OpRegion::parse(&bad).err(), Some(Malformed::Signature));
        let mut bad = region(0b1111);
        bad[SIZE_AT] = 16;
        assert_eq!(OpRegion::parse(&bad).err(), Some(Malformed::Size(16)));
        assert_eq!(OpRegion::parse(&region(0b0111)).err(), Some(Malformed::Mailboxes(0b0111)), "no VBT");
        assert!(OpRegion::parse(&region(1 << 5 | 0b1000)).is_ok(), "a mailbox a later revision defines");
    }
}
