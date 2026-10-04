use super::{DirectMap, PAGE_2M};
use crate::sync::Lock;
use crate::MemoryMapEntry;

/// Region of physical memory to exclude from the free list.
#[derive(Debug, Clone, Copy)]
pub struct Region {
    pub start: u64,
    pub end: u64,
}

/// Owns one 2MB physical page; dropping it returns the page to the free list.
pub struct PhysPage {
    phys: u64,       // raw physical address, 2MB-aligned
}

impl PhysPage {
    /// Caller must ensure `phys` is a previously allocated, 2MB-aligned page.
    pub(super) fn from_raw(phys: u64) -> Self {
        Self { phys }
    }

    /// Access this page through the kernel direct map.
    pub fn direct_map(&self) -> super::DirectMap {
        super::DirectMap::from_phys(self.phys)
    }

}

impl Drop for PhysPage {
    fn drop(&mut self) {
        free_page(self.phys);
    }
}

/// Maximum physical memory: 64 GB → 32768 2MB pages → 4096 bytes bitmap.
const MAX_PAGES: usize = 32768;

struct Bitmap {
    /// One bit per 2MB page. 1 = free, 0 = allocated.
    bits: [u64; MAX_PAGES / 64],
    /// Live user-copy windows covering each page; a non-zero count is never handed out by a scan, so a frame a syscall is mid-copy on cannot be reissued under it.
    pins: [u32; MAX_PAGES],
    /// Physical address of page index 0 (lowest usable page).
    base: u64,
    /// Number of valid page indices (base..base + page_count * PAGE_2M).
    page_count: usize,
    free_count: usize,
    total_usable: usize,
    /// Hint for next free page scan — avoids re-scanning already-allocated prefix.
    next_hint: usize,
}

impl Bitmap {
    const fn new() -> Self {
        Self {
            bits: [0; MAX_PAGES / 64],
            pins: [0; MAX_PAGES],
            base: 0,
            page_count: 0,
            free_count: 0,
            total_usable: 0,
            next_hint: 0,
        }
    }

    /// The managed frame indices the byte range `[phys, phys + len)` touches; empty for a range outside this bitmap, which the PMM never allocates.
    fn frame_span(&self, phys: u64, len: usize) -> core::ops::Range<usize> {
        let lo = phys.max(self.base);
        let hi = (phys + len as u64).min(self.base + self.page_count as u64 * PAGE_2M);
        if lo >= hi {
            return 0..0;
        }
        (((lo - self.base) / PAGE_2M) as usize)..(((hi - 1 - self.base) / PAGE_2M) as usize + 1)
    }

    fn set_free(&mut self, idx: usize) {
        self.bits[idx / 64] |= 1u64 << (idx % 64);
    }

    fn set_used(&mut self, idx: usize) {
        self.bits[idx / 64] &= !(1u64 << (idx % 64));
    }

    fn is_free(&self, idx: usize) -> bool {
        self.bits[idx / 64] & (1u64 << (idx % 64)) != 0
    }

    fn phys_to_idx(&self, phys: u64) -> usize {
        ((phys - self.base) / PAGE_2M) as usize
    }

    fn idx_to_phys(&self, idx: usize) -> u64 {
        self.base + idx as u64 * PAGE_2M
    }
}

static BITMAP: Lock<Bitmap> = Lock::new(Bitmap::new());

/// Initialize the bitmap from the UEFI memory map.
pub(super) fn init(entries: &[MemoryMapEntry], reserved: &[Region]) {
    let mut bm = BITMAP.lock();

    let mut lo = u64::MAX;
    let mut hi = 0u64;
    for entry in entries.iter().filter(|e| is_usable(e)) {
        let start = (entry.start + PAGE_2M - 1) & !(PAGE_2M - 1);
        let end = entry.end & !(PAGE_2M - 1);
        if start < end {
            lo = lo.min(start);
            hi = hi.max(end);
        }
    }
    if lo >= hi { return; }

    bm.base = lo;
    bm.page_count = ((hi - lo) / PAGE_2M) as usize;
    assert!(bm.page_count <= MAX_PAGES, "pmm: physical memory exceeds {} GB", MAX_PAGES * 2 / 1024);

    let mut usable_entries = 0u64;
    let mut firmware_bytes = 0u64;
    let mut withheld = 0u64;
    for entry in entries.iter().filter(|e| is_usable(e)) {
        usable_entries += 1;
        firmware_bytes += entry.end - entry.start;
        let start = (entry.start + PAGE_2M - 1) & !(PAGE_2M - 1);
        let end = entry.end & !(PAGE_2M - 1);
        let mut addr = start;
        while addr + PAGE_2M <= end {
            if !overlaps_reserved(addr, addr + PAGE_2M, reserved) {
                let idx = bm.phys_to_idx(addr);
                bm.set_free(idx);
                bm.free_count += 1;
                bm.total_usable += 1;
            } else {
                withheld += 1;
            }
            addr += PAGE_2M;
        }
    }

    // **Exact, and it has to be**: every byte the firmware called usable is
    // either a whole 2 MiB frame this bitmap manages, a frame withheld because
    // a reserved region touches it, or a fragment lost to the alignment of an
    // entry that does not begin and end on a 2 MiB boundary. Nothing else can
    // become of one, so the three sum to the first, and a reader who cannot
    // reproduce that sum is reading a defect.
    let managed = bm.total_usable as u64 * PAGE_2M;
    let withheld_bytes = withheld * PAGE_2M;
    let (frames, base, span) = (bm.total_usable, bm.base, bm.page_count);
    // The record is written with the lock released: `log!`'s own path may reach
    // the allocator, and the allocator's page comes from this bitmap.
    drop(bm);
    // **Saturating, because the map this arithmetic is over is firmware's and
    // firmware is not trusted** (`crate::drivers::acpi`'s rule, and the same
    // tables). Overlapping usable entries make the accounted total exceed the
    // firmware's own, and a subtraction that panicked here would take the
    // machine down before it has a console to say so on. A zero here is a sum
    // that does not balance, which is what the record claims and what
    // `pmm_accounting` reads it for.
    crate::log!(
        "pmm: the firmware map calls {firmware_bytes} bytes usable in {usable_entries} entries; \
         managed={managed} withheld={withheld_bytes} unaligned={}, and the three sum to it; \
         frames={frames} reserved_frames={withheld} base={base:#x} span={span}",
        firmware_bytes.saturating_sub(managed).saturating_sub(withheld_bytes),
    );
}

/// Allocate one 2MB physical page.
pub fn alloc_page() -> Option<PhysPage> {
    let page = claim()?;
    // SAFETY: `claim` just took the frame off the bitmap, so it is unaliased, and the direct map covers every address the bitmap can name.
    unsafe {
        core::ptr::write_bytes(page.direct_map().as_mut_ptr::<u8>(), 0, PAGE_2M as usize);
    }
    Some(page)
}

/// One 2MB physical page holding what its last owner left in it. Only the kernel heap takes one as it is: the heap answers uninitialized memory, and `alloc_zeroed` writes its own zeros. Does not heap-allocate: the heap calls it when it is full.
pub(super) fn claim() -> Option<PhysPage> {
    let mut bm = BITMAP.lock();
    if bm.free_count == 0 { return None; }
    let start = bm.next_hint;
    for offset in 0..bm.page_count {
        let idx = (start + offset) % bm.page_count;
        if bm.is_free(idx) && bm.pins[idx] == 0 {
            bm.set_used(idx);
            bm.free_count -= 1;
            bm.next_hint = if idx + 1 < bm.page_count { idx + 1 } else { 0 };
            let phys = bm.idx_to_phys(idx);
            drop(bm);
            return Some(PhysPage { phys });
        }
    }
    None
}

/// Allocate `count` physically contiguous 2MB pages.
pub fn alloc_contiguous(count: usize) -> Option<alloc::vec::Vec<PhysPage>> {
    // `count` comes from userland, so a bogus 0 and a legitimate `mmap(0)` can't be told apart here — refuse, don't assert.
    if count == 0 { return None; }
    let mut bm = BITMAP.lock();
    if bm.free_count < count { return None; }

    let mut run = 0usize;
    let mut run_start = 0usize;
    for idx in 0..bm.page_count {
        if bm.is_free(idx) && bm.pins[idx] == 0 {
            if run == 0 { run_start = idx; }
            run += 1;
            if run == count {
                for i in run_start..run_start + count {
                    bm.set_used(i);
                }
                bm.free_count -= count;
                let base_phys = bm.idx_to_phys(run_start);
                drop(bm);
                let mut pages = alloc::vec::Vec::with_capacity(count);
                for i in 0..count {
                    let phys = base_phys + i as u64 * PAGE_2M;
                    // SAFETY: every index in this run was just `set_used` under `BITMAP`'s lock, so it is unaliased and within the direct map.
                    unsafe {
                        core::ptr::write_bytes(
                            DirectMap::from_phys(phys).as_mut_ptr::<u8>(), 0, PAGE_2M as usize,
                        );
                    }
                    pages.push(PhysPage { phys });
                }
                return Some(pages);
            }
        } else {
            run = 0;
        }
    }
    None
}

/// Returns a page to the free bitmap.
fn free_page(phys: u64) {
    let mut bm = BITMAP.lock();
    let idx = bm.phys_to_idx(phys);
    assert!(!bm.is_free(idx), "double free of physical page at {:#x}", phys);
    bm.set_free(idx);
    bm.free_count += 1;
    bm.next_hint = bm.next_hint.min(idx);
}

/// Pin every managed frame `[phys, phys + len)` touches so no scan reissues one while a user-copy window covers it; `false`, and nothing pinned, on a pin-count overflow.
pub fn pin_range(phys: u64, len: usize) -> bool {
    if len == 0 {
        return true;
    }
    let mut bm = BITMAP.lock();
    let span = bm.frame_span(phys, len);
    for idx in span.clone() {
        if bm.pins[idx] == u32::MAX {
            for undo in span.start..idx {
                bm.pins[undo] -= 1;
            }
            return false;
        }
        bm.pins[idx] += 1;
    }
    true
}

/// Release a pin [`pin_range`] took over the same range.
pub fn unpin_range(phys: u64, len: usize) {
    if len == 0 {
        return;
    }
    let mut bm = BITMAP.lock();
    let span = bm.frame_span(phys, len);
    for idx in span {
        bm.pins[idx] = bm.pins[idx]
            .checked_sub(1)
            .expect("unpin of a frame that was not pinned");
    }
}

/// One past the highest physical frame this kernel manages; the IOMMU identity domain's exclusive upper bound.
/// Taken from the bitmap, not the firmware memory map, whose backing buffer is ordinary free RAM by the time anything asks.
pub fn top() -> u64 {
    let bm = BITMAP.lock();
    bm.base + bm.page_count as u64 * PAGE_2M
}

/// Return (total_bytes, used_bytes).
pub fn stats() -> (u64, u64) {
    let bm = BITMAP.lock();
    let total = bm.total_usable as u64 * PAGE_2M;
    let used = (bm.total_usable - bm.free_count) as u64 * PAGE_2M;
    (total, used)
}

fn is_usable(entry: &MemoryMapEntry) -> bool {
    toyos_bootmap::is_usable_type(entry.uefi_type)
}

fn overlaps_reserved(start: u64, end: u64, reserved: &[Region]) -> bool {
    reserved.iter().any(|r| start < r.end && end > r.start)
}
