//! Kernel stacks, each above memory that is not mapped, so running off its
//! bottom faults instead of writing memory another owner holds.
//!
//! **A thread's** is a [`Stack`]: frames its job holds, mapped at the top of a
//! slot of a kernel virtual range of their own ([`RANGE`]). The rest of every
//! slot is never mapped in any incarnation, so the guard is true on every CPU
//! with nothing to flush. A dropped stack's frames go back at once and its
//! slot's address waits (`kernel::stackslots`): until the flush, a stale
//! translation of it reaches only what a kernel use-after-free of the dead
//! stack would, the exposure every frame already has through the direct map.
//! The range's page tables are the kernel's own and are never freed, since a
//! CPU's paging-structure cache may still name one: a table for each 8 slots
//! ever used and a directory for each 4096, `kernel::stackslots` bounding the
//! slots ever used and the stacks the range holds at once.
//!
//! **A CPU's own** (idle, x86-64's IST) is [`permanent`]: carved at bring-up
//! out of the direct map, above a page taken out of it, because an x86-64 AP
//! runs on its idle stack under the loader's map, which holds no other
//! range. The boot stack's guard is the image's ([`super::Image`]).

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ops::Range;

use kernel::sched::job::Job;
use kernel::stackslots::{Slots, Take};

use super::{DirectMap, PAGE_2M, PAGE_BYTES, PAGE_SIZE};
use crate::invalidation::{Half, Origin};
use crate::sync::Lock;

/// The kernel's root slot 510, which the direct map never reaches
/// ([`super::init`] refuses a machine whose memory would).
pub const RANGE: u64 = 0xFFFF_FF00_0000_0000;

/// One slot: a stack at its top and nothing below it. Eight to a table of
/// 4 KiB leaves, so no slot crosses one.
const SLOT: u64 = 256 * 1024;

/// Which slots are in use, clean or waiting on a flush.
static IN_USE: Lock<Slots> = Lock::new(Slots::new());

/// Held by the one taker flushing, across its flush. A taker that finds no
/// clean slot beside it waits here behind those ahead of it: each hold is
/// one shootdown, which ends or panics on a CPU that does not answer, and a
/// spinner on this lock answers it.
static FLUSH_GATE: Lock<()> = Lock::new(());

/// A slot no CPU holds a translation of, or `None` where every slot the
/// range holds is in use: a refused spawn.
fn take_slot() -> Option<u32> {
    loop {
        // Bound to a name so the lock is let go before the gate is taken.
        let take = IN_USE.lock().take();
        match take {
            Take::Slot(slot) => return Some(slot),
            Take::Spent => return None,
            Take::Flush => {
                let _gate = FLUSH_GATE.lock();
                if IN_USE.lock().begin_flush() {
                    // Every CPU drops every translation of the kernel half.
                    // AArch64's unmap already broadcast each page's, and
                    // this does nothing there.
                    crate::arch::tlb::shootdown(Origin::Stack, Half::Kernel);
                    IN_USE.lock().flushed();
                }
            }
        }
    }
}

/// Where the `pages`-page stack in `slot` begins: it ends with its slot.
fn bottom_of(slot: u32, pages: usize) -> u64 {
    RANGE + (u64::from(slot) + 1) * SLOT - (pages * PAGE_BYTES) as u64
}

/// A thread's kernel stack: zeroed frames `job` holds, which go back to it
/// when this drops.
pub struct Stack {
    slot: u32,
    pages: usize,
    job: Arc<Job>,
}

impl Stack {
    /// `bytes` in whole pages for `job`, or `None` where it may hold no more
    /// frames, no table can be had for the slot, or every slot is in use.
    pub fn new(job: &Arc<Job>, bytes: usize) -> Option<Self> {
        let pages = bytes / PAGE_BYTES;
        assert!(bytes.is_multiple_of(PAGE_BYTES) && pages > 0 && (pages as u64) < SLOT / PAGE_SIZE, "stack: {bytes} bytes");
        let mut frames = Vec::new();
        frames.try_reserve_exact(pages).ok()?;
        if !super::pmm::alloc_held(job, pages, |frame| frames.push(frame)) {
            return None;
        }
        for &frame in &frames {
            // SAFETY: a frame just taken off the allocator, through the direct map.
            unsafe { core::ptr::write_bytes(DirectMap::from_phys(frame).as_mut_ptr::<u8>(), 0, PAGE_BYTES) };
        }
        let slot = take_slot();
        match slot {
            Some(slot) if super::paging::map_stack(bottom_of(slot, pages), &frames) => {
                Some(Self { slot, pages, job: Arc::clone(job) })
            }
            unmapped => {
                // SAFETY: no table names the frames.
                unsafe { super::pmm::free_held(job, frames) };
                if let Some(slot) = unmapped {
                    IN_USE.lock().unused(slot);
                }
                None
            }
        }
    }

    /// The lowest byte.
    pub fn bottom(&self) -> *mut u8 {
        bottom_of(self.slot, self.pages) as *mut u8
    }
}

impl Drop for Stack {
    fn drop(&mut self) {
        let mut frames = [0u64; (SLOT / PAGE_SIZE) as usize];
        let mut count = 0;
        super::paging::unmap_stack(self.bottom() as u64, self.pages, |frame| {
            frames[count] = frame;
            count += 1;
        });
        // SAFETY: their leaves are cleared and the tables above them stay, so
        // no walk reaches one as a table. A translation another CPU still
        // holds is the module's stated exposure, and the slot is not reused
        // until it is flushed.
        unsafe { super::pmm::free_held(&self.job, frames[..count].iter().copied()) };
        IN_USE.lock().retire(self.slot);
    }
}

/// The unmapped memory under the stack whose lowest byte is at `bottom`: the
/// rest of its slot for a [`Stack`], the page below for a [`permanent`] one.
pub fn guard_under(bottom: u64) -> Range<u64> {
    let below = bottom.wrapping_sub(1);
    let start = if below >= RANGE { below & !(SLOT - 1) } else { bottom.wrapping_sub(PAGE_SIZE) };
    start..bottom
}

/// What is left of the superframe [`permanent`] is carving, by direct-map
/// address.
static CARVING: Lock<(u64, usize)> = Lock::new((0, 0));

/// The top of a `bytes`-byte stack that is never dropped, above a page taken
/// out of the direct map. Bring-up's alone, on the boot CPU: it panics where
/// there is no memory, and the hole stays for the machine's life.
pub fn permanent(bytes: usize) -> u64 {
    let slot = PAGE_BYTES + bytes;
    assert!(bytes > 0 && bytes.is_multiple_of(PAGE_BYTES) && slot <= PAGE_2M as usize, "stack: {bytes} bytes");
    let guard = {
        let mut carving = CARVING.lock();
        if carving.1 < slot {
            let page = super::pmm::alloc_page().expect("stack: no memory for a CPU's own stack");
            *carving = (page.direct_map().as_mut_ptr::<u8>() as u64, PAGE_2M as usize);
            #[expect(clippy::disallowed_methods, reason = "its stacks' guards are holes in the direct map, so it is never given back")]
            core::mem::forget(page);
        }
        let guard = carving.0;
        *carving = (guard + slot as u64, carving.1 - slot);
        guard
    };
    super::paging::guard_4k(DirectMap::phys_of(guard as *const u8));
    // A CPU already up may still translate the guard through the
    // superframe's 2 MiB leaf.
    crate::arch::tlb::shootdown(Origin::Guard, Half::Kernel);
    guard + slot as u64
}
