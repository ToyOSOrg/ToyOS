//! Kernel stacks, each above memory that is not mapped, so running off its
//! bottom faults instead of writing memory another owner holds.
//!
//! **A thread's** is a [`Stack`]: frames its job holds, mapped at the top of a
//! slot of a kernel virtual range of their own ([`RANGE`]). The rest of every
//! slot is never mapped in any incarnation, so the guard is true on every CPU
//! with nothing to flush. A dropped stack's frames go back at once and its
//! slot's address waits ([`FLUSH_BATCH`]): until the flush, a stale
//! translation of it reaches only what a kernel use-after-free of the dead
//! stack would, the exposure every frame already has through the direct map.
//! The range's page tables are the kernel's own and are never freed, since a
//! CPU's paging-structure cache may still name one: a table for each 8 slots
//! ever in use at once and a directory for each 4096.
//!
//! **A CPU's own** (idle, x86-64's IST) is [`permanent`]: carved at bring-up
//! out of the direct map, above a page taken out of it, because an x86-64 AP
//! runs on its idle stack under the loader's map, which holds no other
//! range. The boot stack's guard is the image's ([`super::Image`]).

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ops::Range;

use kernel::sched::job::Job;

use super::{DirectMap, PAGE_2M, PAGE_BYTES, PAGE_SIZE};
use crate::invalidation::{Half, Origin};
use crate::sync::Lock;

/// The kernel's root slot 510, which the direct map never reaches
/// ([`super::init`] refuses a machine whose memory would).
pub const RANGE: u64 = 0xFFFF_FF00_0000_0000;

/// One slot: a stack at its top and nothing below it. Eight to a table of
/// 4 KiB leaves, so no slot crosses one.
const SLOT: u64 = 256 * 1024;

/// How many slots the range's 512 GiB holds.
const SLOTS: u32 = ((1u64 << 39) / SLOT) as u32;

/// Retired slots a spawn waits for before it flushes them clean rather than
/// take a slot never used. A flush interrupts every CPU, so a spawn pays one
/// in at most this many; and this many slots' addresses, eight tables' worth,
/// are the most that wait unused.
const FLUSH_BATCH: usize = 64;

struct Slots {
    /// Slots below this have been used.
    fresh: u32,
    /// Used slots no CPU holds a translation of.
    clean: Vec<u32>,
    /// Slots whose stack dropped since the last flush.
    retired: Vec<u32>,
    /// A spawn is flushing `retired`'s head, outside the lock.
    flushing: bool,
}

static SLOTS_FREE: Lock<Slots> = Lock::new(Slots { fresh: 0, clean: Vec::new(), retired: Vec::new(), flushing: false });

/// A slot no CPU holds a translation of, or `None` where the range is spent
/// or the lists cannot grow: a refused spawn.
fn take_slot() -> Option<u32> {
    let mut slots = SLOTS_FREE.lock();
    // Room for every used slot in either list, so a drop never allocates.
    let used = slots.fresh as usize + 1;
    let (clean, retired) = (used - slots.clean.len(), used - slots.retired.len());
    slots.clean.try_reserve(clean).ok()?;
    slots.retired.try_reserve(retired).ok()?;
    if let Some(slot) = slots.clean.pop() {
        return Some(slot);
    }
    let waiting = slots.retired.len();
    let spent = slots.fresh == SLOTS;
    if !slots.flushing && (waiting >= FLUSH_BATCH || (spent && waiting > 0)) {
        slots.flushing = true;
        drop(slots);
        // Every CPU drops every translation of the kernel half, so the
        // `waiting` slots retired before it are clean. AArch64's unmap
        // already broadcast each page's, and this does nothing there.
        crate::arch::tlb::shootdown(Origin::Stack, Half::Kernel);
        let mut slots = SLOTS_FREE.lock();
        slots.flushing = false;
        let slots = &mut *slots;
        slots.clean.extend(slots.retired.drain(..waiting));
        return slots.clean.pop();
    }
    if spent {
        return None;
    }
    slots.fresh += 1;
    Some(slots.fresh - 1)
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
    /// frames, no table can be had for the slot, or the range is spent.
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
        let stack = take_slot().map(|slot| Self { slot, pages, job: Arc::clone(job) });
        match stack {
            Some(stack) if super::paging::map_stack(stack.bottom() as u64, &frames) => Some(stack),
            unmapped => {
                // SAFETY: no table names the frames. A slot taken and not
                // mapped is still clean.
                unsafe { super::pmm::free_held(job, frames) };
                if let Some(stack) = unmapped {
                    SLOTS_FREE.lock().clean.push(stack.slot);
                    #[expect(clippy::disallowed_methods, reason = "it holds no frame, and its slot went back above")]
                    core::mem::forget(stack);
                }
                None
            }
        }
    }

    /// One past the highest byte: its slot's end.
    pub fn top(&self) -> u64 {
        RANGE + (u64::from(self.slot) + 1) * SLOT
    }

    /// The lowest byte.
    pub fn bottom(&self) -> *mut u8 {
        (self.top() - (self.pages * PAGE_BYTES) as u64) as *mut u8
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
        SLOTS_FREE.lock().retired.push(self.slot);
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
