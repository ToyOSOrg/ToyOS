//! Kernel stacks. Every stack the kernel runs on once it has tables of its
//! own is a [`Stack`]: whole pages above one page the direct map does not
//! hold, so running off its bottom faults instead of writing memory another
//! owner holds. The boot stack's guard is the image's ([`super::Image`]).
//!
//! Stacks are carved out of superframes taken as the heap takes its own, so a
//! [`Charge`](super::charge::Charge) covers one as it covers a heap
//! allocation. A dropped stack is kept for the next of its size: the hole
//! under it stays in the direct map, so its memory never goes back to the
//! frame allocator.

use alloc::vec::Vec;
use core::ops::Range;

use super::pmm::HeapPage;
use super::{DirectMap, PAGE_2M, PAGE_BYTES};
use crate::invalidation::{Half, Origin};
use crate::sync::Lock;

/// The unmapped page under every stack.
pub const GUARD: usize = PAGE_BYTES;

/// The guard page under a `bytes`-byte stack ending at `top`.
pub fn guard_below(top: u64, bytes: usize) -> Range<u64> {
    let bottom = top.wrapping_sub(bytes as u64);
    bottom.wrapping_sub(GUARD as u64)..bottom
}

struct Pool {
    /// Direct-map address of what is left of the superframe being carved.
    next: u64,
    left: usize,
    /// Every stack ever carved; `free` holds room for them all, so a drop
    /// never allocates.
    carved: usize,
    /// Dropped stacks: `(bytes, bottom)`.
    free: Vec<(usize, u64)>,
}

static POOL: Lock<Pool> = Lock::new(Pool { next: 0, left: 0, carved: 0, free: Vec::new() });

/// A zeroed stack over its guard page, kept for reuse when it drops.
pub struct Stack {
    bottom: u64,
    bytes: usize,
}

impl Stack {
    /// `bytes` in whole pages, or `None` where there is no memory for one.
    pub fn new(bytes: usize) -> Option<Self> {
        let slot = GUARD + bytes;
        assert!(bytes > 0 && bytes.is_multiple_of(PAGE_BYTES) && slot <= PAGE_2M as usize, "stack: {bytes} bytes");
        let mut pool = POOL.lock();
        if let Some(at) = pool.free.iter().rposition(|&(size, _)| size == bytes) {
            let (_, bottom) = pool.free.swap_remove(at);
            drop(pool);
            // SAFETY: the stack was dropped, so nothing else holds its bytes.
            unsafe { core::ptr::write_bytes(bottom as *mut u8, 0, bytes) };
            return Some(Self { bottom, bytes });
        }
        let room = pool.carved + 1 - pool.free.len();
        pool.free.try_reserve(room).ok()?;
        if pool.left < slot {
            let page = HeapPage::grow()?;
            pool.next = page.direct_map().as_mut_ptr::<u8>() as u64;
            pool.left = PAGE_2M as usize;
            #[expect(clippy::disallowed_methods, reason = "its stacks' guards are holes in the direct map, so it is never given back")]
            core::mem::forget(page);
        }
        let guard = pool.next;
        pool.next += slot as u64;
        pool.left -= slot;
        pool.carved += 1;
        super::paging::guard_4k(DirectMap::phys_of(guard as *const u8));
        drop(pool);
        // Another CPU may still translate the guard through the superframe's
        // 2 MiB leaf.
        crate::arch::tlb::shootdown(Origin::Guard, Half::Kernel);
        Some(Self { bottom: guard + GUARD as u64, bytes })
    }

    /// The top of a stack that is never dropped: a CPU's own.
    pub fn permanent(bytes: usize) -> u64 {
        let stack = Self::new(bytes).expect("stack: no memory for a CPU's own stack");
        let top = stack.top();
        #[expect(clippy::disallowed_methods, reason = "a CPU runs on it for the machine's life")]
        core::mem::forget(stack);
        top
    }

    /// The lowest byte.
    pub fn bottom(&self) -> *mut u8 {
        self.bottom as *mut u8
    }

    /// One past the highest byte.
    pub fn top(&self) -> u64 {
        self.bottom + self.bytes as u64
    }
}

impl Drop for Stack {
    fn drop(&mut self) {
        POOL.lock().free.push((self.bytes, self.bottom));
    }
}
