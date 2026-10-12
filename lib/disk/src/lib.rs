//! A partition as a program that reads and writes its blocks holds it, in
//! whole 4 KiB blocks, whoever serves it: the one trait the block service's
//! sessions, a claim on the kernel's disk and every test's memory implement.
//!
//! **A block is 4 KiB here, on every source.** A partition that is not whole
//! 4 KiB blocks is refused before it is served, so nothing below has a second
//! block size.

pub const BLOCK: usize = 4096;

/// Why a disk did not do what it was asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiskError {
    /// The device's own word: the transfer was attempted and did not happen,
    /// or a flush left writes the device would not keep.
    Device,
    /// The block service is gone and would not come back.
    Gone,
    /// Past the end of the partition.
    Range,
    /// A write the block service's grant to this holder does not make.
    ReadOnly,
}

/// A partition, a block at a time or several.
pub trait Disk {
    fn blocks(&self) -> u64;
    /// `out.len()` is whole blocks.
    fn read(&mut self, first: u64, out: &mut [u8]) -> Result<(), DiskError>;
    /// `data.len()` is whole blocks; durable once [`Disk::flush`] answers `Ok`.
    fn write(&mut self, first: u64, data: &[u8]) -> Result<(), DiskError>;
    /// Every write answered before this, out of the device's cache.
    fn flush(&mut self) -> Result<(), DiskError>;
}

/// How many blocks `len` bytes from `first` are, or `Range` past the end of
/// a partition `blocks` long.
pub fn span(first: u64, len: usize, blocks: u64) -> Result<u64, DiskError> {
    assert!(len.is_multiple_of(BLOCK), "a transfer of {len} bytes is no whole blocks");
    let count = (len / BLOCK) as u64;
    match first.checked_add(count) {
        Some(end) if end <= blocks => Ok(count),
        _ => Err(DiskError::Range),
    }
}
