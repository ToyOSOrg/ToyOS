//! Every CPU's idle stack: one per CPU, the size of a task's kernel stack, above
//! a guard page the direct map does not hold, so running off its bottom is a
//! fault rather than a write into memory another execution owns.
//!
//! **Taken from 2 MiB pages of their own, not the kernel heap**: the guard's
//! hole in the direct map splits the page that holds it, and a heap page split
//! so would cost every other allocation in it its 2 MiB translation. Never
//! freed — a page returned to the PMM would keep the hole.

use alloc::vec::Vec;

use crate::mm::DirectMap;
use crate::sync::Lock;

/// Same size as a task's kernel stack: a `deferred` [`kobject!`] object may
/// own an `immediate` one, whose destructor then runs here instead.
pub const SIZE: usize = crate::process::KERNEL_STACK_SIZE;

/// One unmapped 4 KiB page below every idle stack.
const GUARD: usize = crate::mm::PAGE_BYTES;

/// One idle stack and the guard page under it.
const SLOT: usize = GUARD + SIZE;

static ARENA: Lock<Arena> = Lock::new(Arena { pages: Vec::new(), next: 0, left: 0 });

struct Arena {
    pages: Vec<crate::mm::pmm::PhysPage>,
    /// Direct-map address of the next free slot.
    next: u64,
    left: usize,
}

/// A 4 KiB-aligned [`SLOT`] from the arena.
fn alloc_slot() -> u64 {
    let mut arena = ARENA.lock();
    if arena.left < SLOT {
        let page = crate::mm::pmm::alloc_page()
            .expect("idle stack: no physical page for one");
        arena.next = page.direct_map().as_mut_ptr::<u8>() as u64;
        arena.left = crate::mm::PAGE_2M as usize;
        arena.pages.push(page);
    }
    let base = arena.next;
    arena.next += SLOT as u64;
    arena.left -= SLOT;
    base
}

/// A fresh idle stack over its unmapped guard page: its top.
pub fn alloc() -> u64 {
    let base = alloc_slot();
    crate::mm::paging::kernel().lock().guard_4k(DirectMap::phys_of(base as *const u8));
    base + SLOT as u64
}
