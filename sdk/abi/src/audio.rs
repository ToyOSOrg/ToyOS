//! The shared memory protocol soundserver serves clients.

use core::sync::atomic::AtomicU32;

/// Shared memory header for the client↔soundserver slot-ring protocol.
///
/// Client increments `write_idx` after filling a slot; soundserver increments
/// `read_idx` after it has finished mixing from one. Ring is full when
/// `write_idx - read_idx >= slot_count` (slot_count arrives in
/// `MSG_STREAM_OPENED`). The two indices live on separate cache lines:
/// they are written by different processes at audio-period rate.
#[repr(C, align(64))]
pub struct AudioSlotHeader {
    pub write_idx: AtomicU32,
    _pad0: [u32; 15],
    pub read_idx: AtomicU32,
    _pad1: [u32; 15],
}

impl AudioSlotHeader {
    pub const SIZE: usize = core::mem::size_of::<Self>();
}
