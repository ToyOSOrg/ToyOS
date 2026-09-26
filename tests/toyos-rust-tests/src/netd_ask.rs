//! The request that opens every netd stream test's connection to the
//! harness's host peer (`tests/common/tcppeer.rs`), declared once for both
//! sides: a mode byte, then the length and the seed of the stream, each eight
//! little-endian bytes. The guest's stream tests and the host peer each
//! include this file whole.
#![allow(dead_code)]

/// What a connection asks the host peer to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Mode {
    /// Send `len` bytes of the stream `seed` names, then FIN.
    Download,
    /// Read to the end of the stream, then answer its length and hash, then FIN.
    Upload,
    /// Send `len` bytes, wait for the guest's byte saying they arrived, then reset.
    Reset,
    /// Read nothing and send nothing until the peer finishes.
    Hold,
    /// Send `len` bytes of [`stream_byte`]'s pattern, then FIN.
    Pattern,
    /// [`Mode::Upload`], reading nothing until a [`Mode::Release`] names the seed.
    LateUpload,
    /// Let the [`Mode::LateUpload`] with this seed start reading.
    Release,
    /// [`Mode::Pattern`]'s bytes, and then [`Mode::Hold`] with no FIN.
    PatternHeld,
    /// Dial the guest's forwarded port and write until refused; then FIN.
    Dial,
    /// Tell the wire (`tests/common/middlebox.rs`'s `Plan::Dark`) to carry
    /// nothing more either way, ARP included.
    Dark,
}

impl Mode {
    /// The mode a request's first byte names.
    pub fn of(byte: u8) -> Option<Self> {
        use Mode::*;
        let all = [Download, Upload, Reset, Hold, Pattern, LateUpload, Release, PatternHeld, Dial, Dark];
        all.get(byte as usize).copied().filter(|mode| *mode as u8 == byte)
    }

    /// This mode over `len` bytes of the stream `seed` names, on the wire.
    pub fn request(self, len: u64, seed: u64) -> [u8; REQUEST_LEN] {
        let mut request = [self as u8; REQUEST_LEN];
        request[1..9].copy_from_slice(&len.to_le_bytes());
        request[9..].copy_from_slice(&seed.to_le_bytes());
        request
    }
}

/// A request's length on the wire.
pub const REQUEST_LEN: usize = 17;

/// The seeds of `netd_tcp`'s streams the host's record of is found by: the
/// `reader_leaves` stream, the `late_shutdown` upload, and the `orphan_owes`
/// upload whose window opens.
pub const READER_LEAVES_SEED: u64 = 77;
pub const LATE_SHUTDOWN_SEED: u64 = 19;
pub const ORPHAN_RELEASED_SEED: u64 = 91;

/// Byte at absolute stream position `pos` of [`Mode::Pattern`]'s stream.
/// Every aligned 16-byte group carries its own index, so a lost, duplicated
/// or reordered run shows up whether it is a multiple of 16 long (wrong
/// stamp) or not (wrong filler).
pub fn stream_byte(pos: u64) -> u8 {
    let group = (pos >> 4) as u32;
    match pos & 15 {
        k @ 0..=3 => (group >> (8 * k)) as u8,
        _ => 0xC3,
    }
}
