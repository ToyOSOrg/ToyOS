//! The `/home` filesystem. [`upstream`] implements the real bcachefs on-disk
//! format, read side; the modules beside it are the interim ToyOS format the
//! kernel still links.
//!
//! **Every number this crate parses came off a disk it does not own, and a
//! CRC is not authentication — whoever writes the image writes the CRC.** Each
//! bound is applied where the bytes become typed data and nowhere else: a
//! superblock's fields in `Superblock::check`, a btree child pointer in
//! `Node::parse`, a file's extents in `decode_leaf_value`. A consumer of what
//! those hand back does not re-check, so a new one of them is a new refusal
//! here rather than a comparison at the call site.

#![cfg_attr(not(feature = "std"), no_std)]
#![allow(dead_code)]

extern crate alloc;

mod block_io;
mod crc32c;
mod superblock;
mod alloc_bitmap;
mod btree;
mod fs;
pub mod upstream;

pub use block_io::{BlockIO, BlockBuf, BlockNum, DeviceError, TransferError};
#[cfg(feature = "std")]
pub use block_io::VecBlockIO;
pub use fs::{Formatted, Mounted, ReadOnly, ReadWrite, FsError, Extent};
pub use superblock::{DESIGNATION_BLOCKS_OFFSET, DESIGNATION_MAGIC, FsUuid, Superblock};

/// Records the largest single allocation each test thread makes, so a test can
/// assert what parsing a crafted block asks the allocator for.
///
/// Nothing else can see that. The defect this exists to catch — a `Vec` sized
/// from an on-disk `u16` — leaves the parse returning the same error either
/// way, so a test that only checks the return value passes with the bug in.
#[cfg(test)]
mod alloc_probe {
    use core::alloc::{GlobalAlloc, Layout};
    use core::cell::Cell;
    use std::alloc::System;

    std::thread_local! {
        static PEAK: Cell<usize> = const { Cell::new(0) };
    }

    pub struct Probe;

    fn record(size: usize) {
        // `try_with` because TLS destruction can free after the slot is gone.
        let _ = PEAK.try_with(|p| p.set(p.get().max(size)));
    }

    /// The largest single allocation this thread has made since the last call.
    pub fn take_peak() -> usize {
        PEAK.with(|p| p.replace(0))
    }

    unsafe impl GlobalAlloc for Probe {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            record(layout.size());
            unsafe { System.alloc(layout) }
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            unsafe { System.dealloc(ptr, layout) }
        }
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            record(new_size);
            unsafe { System.realloc(ptr, layout, new_size) }
        }
    }

    #[global_allocator]
    static PROBE: Probe = Probe;
}
