//! What a claim on one GPT partition names and hands its holder.
//!
//! A partition is claimed the way a PCI function is: `/bin/init` mints it from a
//! `system.toml` `devices` entry and endows it, and the holder reads and writes
//! the partition's blocks through [`syscall::partition_read`] and
//! [`syscall::partition_write`], durable at [`syscall::fsync`] on the claim.
//! Every block number here is the **partition's own**, from 0: nothing in this
//! ABI names a device block, so a neighbour's blocks have no spelling.
//!
//! A partition has one holder. Which partitions the kernel holds itself, and so
//! refuses, is `kernel/src/block.rs`'s account.
//!
//! [`syscall::partition_read`]: crate::syscall::partition_read
//! [`syscall::partition_write`]: crate::syscall::partition_write
//! [`syscall::fsync`]: crate::syscall::fsync

/// The unit every partition transfer is in, and the alignment a partition must
/// have on its device to be claimable at all: a partition that began or ended
/// inside one of these would share a transfer with its neighbour.
pub const BLOCK_BYTES: usize = 4096;

/// One partition block, as the buffers of [`crate::syscall::partition_read`]
/// and [`crate::syscall::partition_write`] are typed: a partial block has no
/// spelling.
pub type Block = [u8; BLOCK_BYTES];

/// The most blocks one transfer call carries.
pub const MAX_BLOCKS_PER_CALL: usize = 32;

/// A GUID as the sixteen bytes a GPT entry stores (UEFI 2.10 §5.3.3), which is
/// also the order the claim syscall carries it in.
///
/// The text form is the registry form, `XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX`
/// in **uppercase** hex: the first three groups are little-endian integers on
/// the disk and the last two are bytes in order (UEFI 2.10 Appendix A). One
/// spelling per partition, because every reader of a `devices` entry compares
/// the string: a lowercase twin would be a second name for one partition.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PartGuid(pub [u8; 16]);

/// Where each byte of the on-disk form is printed, in text order: the three
/// little-endian groups reversed, the last eight in place.
const TEXT_ORDER: [usize; 16] = [3, 2, 1, 0, 5, 4, 7, 6, 8, 9, 10, 11, 12, 13, 14, 15];

/// The dash positions of the 36-byte text form.
const DASHES: [usize; 4] = [8, 13, 18, 23];

/// The length of the text form.
pub const GUID_TEXT_LEN: usize = 36;

const HEX_UPPER: &[u8; 16] = b"0123456789ABCDEF";

impl PartGuid {
    /// The registry text form, exactly, and nothing else.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.as_bytes();
        if text.len() != GUID_TEXT_LEN {
            return None;
        }
        let mut digits = [0u8; 32];
        let mut n = 0;
        for (at, &byte) in text.iter().enumerate() {
            if DASHES.contains(&at) {
                if byte != b'-' {
                    return None;
                }
                continue;
            }
            digits[n] = match byte {
                b'0'..=b'9' => byte - b'0',
                b'A'..=b'F' => byte - b'A' + 10,
                _ => return None,
            };
            n += 1;
        }
        let mut bytes = [0u8; 16];
        for (i, &at) in TEXT_ORDER.iter().enumerate() {
            bytes[at] = (digits[2 * i] << 4) | digits[2 * i + 1];
        }
        Some(Self(bytes))
    }

    /// The registry text form written into `buf`.
    pub fn write_text(self, buf: &mut [u8; GUID_TEXT_LEN]) -> &str {
        let mut at = 0;
        for (i, &byte) in TEXT_ORDER.iter().map(|&b| &self.0[b]).enumerate() {
            if i == 4 || i == 6 || i == 8 || i == 10 {
                buf[at] = b'-';
                at += 1;
            }
            buf[at] = HEX_UPPER[(byte >> 4) as usize];
            buf[at + 1] = HEX_UPPER[(byte & 0xF) as usize];
            at += 2;
        }
        core::str::from_utf8(buf).expect("hex digits and dashes are ASCII")
    }

    /// The two selector words the claim syscall carries: bytes 0..8 and
    /// 8..16, each little-endian.
    pub fn wire(self) -> [u64; 2] {
        let mut lo = [0u8; 8];
        let mut hi = [0u8; 8];
        lo.copy_from_slice(&self.0[..8]);
        hi.copy_from_slice(&self.0[8..]);
        [u64::from_le_bytes(lo), u64::from_le_bytes(hi)]
    }

    /// The selector words decoded; every pair of words is some GUID.
    pub fn from_wire(words: [u64; 2]) -> Self {
        let mut bytes = [0u8; 16];
        bytes[..8].copy_from_slice(&words[0].to_le_bytes());
        bytes[8..].copy_from_slice(&words[1].to_le_bytes());
        Self(bytes)
    }
}

/// The partition a claim holds, as its holder reads it once off the claim.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PartitionInfo {
    /// The partition's length in [`BLOCK_BYTES`] blocks; block numbers
    /// `0..blocks` are the whole of what the claim addresses.
    pub blocks: u64,
    pub unique_guid: [u8; 16],
}

/// Every byte belongs to a field: this crosses the boundary through
/// `as_bytes`, so a gap would publish whatever the kernel stack held.
const _: () = assert!(core::mem::size_of::<PartitionInfo>() == 8 + 16);

impl PartitionInfo {
    /// The partition's unique GUID, as its `part:` entry names it.
    pub const fn unique(&self) -> PartGuid {
        PartGuid(self.unique_guid)
    }

    pub fn as_bytes(&self) -> &[u8] {
        // SAFETY: `self` is a valid `&Self`, readable for `size_of::<Self>()`
        // bytes, and the const assert above proves the `repr(C)` layout has no
        // padding, so every byte the slice exposes is an initialized field.
        unsafe {
            core::slice::from_raw_parts(
                self as *const Self as *const u8,
                core::mem::size_of::<Self>(),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The EFI System Partition type's registry text and its on-disk bytes, from
    /// the UEFI specification (§5.3.3, Table 5.8): both directions, because a
    /// mixed-endian mistake that is self-consistent survives either one alone.
    #[test]
    fn the_esp_type_guid_is_the_specification_s_bytes() {
        const TEXT: &str = "C12A7328-F81F-11D2-BA4B-00A0C93EC93B";
        const ON_DISK: [u8; 16] = [
            0x28, 0x73, 0x2A, 0xC1, 0x1F, 0xF8, 0xD2, 0x11, 0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E,
            0xC9, 0x3B,
        ];
        assert_eq!(PartGuid::parse(TEXT), Some(PartGuid(ON_DISK)));
        let mut buf = [0u8; GUID_TEXT_LEN];
        assert_eq!(PartGuid(ON_DISK).write_text(&mut buf), TEXT);
    }

    /// One spelling per partition: every near miss a config could plausibly
    /// write is refused, not normalised.
    #[test]
    fn a_guid_is_the_uppercase_registry_form_and_nothing_else() {
        for bad in [
            "c12a7328-f81f-11d2-ba4b-00a0c93ec93b",
            "{C12A7328-F81F-11D2-BA4B-00A0C93EC93B}",
            "C12A7328F81F11D2BA4B00A0C93EC93B",
            "C12A7328-F81F-11D2-BA4B-00A0C93EC93",
            "C12A7328-F81F-11D2-BA4B-00A0C93EC93BB",
            "C12A7328-F81F-11D2-BA4B00-A0C93EC93B",
            "G12A7328-F81F-11D2-BA4B-00A0C93EC93B",
            "C12A7328-F81F-11D2-BA4B-00A0C93EC9é",
            "",
        ] {
            assert_eq!(PartGuid::parse(bad), None, "{bad:?} parsed");
        }
    }

    /// The claim carries the sixteen bytes as two words and the kernel takes
    /// them apart again; a byte lost to a shift claims a different partition.
    #[test]
    fn a_guid_survives_the_wire() {
        let mut every = [0u8; 16];
        for (i, b) in every.iter_mut().enumerate() {
            *b = 0xF0 | i as u8;
        }
        for guid in [PartGuid(every), PartGuid([0; 16]), PartGuid([0xFF; 16])] {
            assert_eq!(PartGuid::from_wire(guid.wire()), guid);
        }
    }
}
