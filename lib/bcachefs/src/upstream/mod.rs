//! The upstream bcachefs on-disk format, read side.
//!
//! Format source: `fs/bcachefs_format.h` and the `*_format.h` beside it in
//! <https://github.com/koverstreet/bcachefs-tools> at commit
//! `d997ad76e906ca9cbe1d4045d3880185e504191d` (tag v1.39.4), whose
//! `bcachefs_metadata_version_current` is `per_dev_fragmentation_lru`,
//! `BCH_VERSION(1, 39)` = 1063 — the version `bcachefs format` writes.
//!
//! **Every number here came off a disk this crate does not own, and a checksum
//! is not authentication — whoever writes the image writes the checksum.** A
//! bound is applied where bytes become typed data and nowhere else, and each
//! refusal names what it refused: [`UpstreamError::Refused`] carries a
//! sentence, never a number a caller has to interpret. Nothing in this module
//! reinterprets disk bytes as a Rust type; every field is read out of a slice
//! at an offset this module states, because the format is packed and a
//! `repr(C)` view of it would be both unaligned and unchecked.
//!
//! This is the read half. It has no allocator, no journal writer and no
//! mutation, so a consumer that links it cannot change a disk.

pub mod btree;
pub mod csum;
pub mod bkey;
pub mod raw;
pub mod fs;
pub mod node;
pub mod sb;

/// What a refusal is: a sentence naming the field or feature, so a log line
/// says which byte on which disk stopped the mount.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpstreamError {
    /// The image says something this reader will not act on.
    Refused(&'static str),
    /// The device would not answer for the block the reader asked for.
    Device(crate::BlockNum, crate::DeviceError),
}
