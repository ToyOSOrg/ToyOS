use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicU8, Ordering};

use super::MAX_HEAP_ALLOC;
use super::PHYS_OFFSET;
use super::PAGE_2M;
use super::pmm;

/// Invariant: no method here may panic — it runs inside `dlmalloc.lock()`, which cannot unwind.
struct KernelPageSource;

// SAFETY: `alloc` only ever hands out whole `PAGE_2M` pages from `pmm::alloc_page`, and `free` is the only path that returns one.
unsafe impl dlmalloc::Allocator for KernelPageSource {
    fn alloc(&self, size: usize) -> (*mut u8, usize, u32) {
        if size > PAGE_2M as usize {
            return (core::ptr::null_mut(), 0, 0);
        }
        if let Some(page) = pmm::alloc_page(pmm::Category::KernelHeap) {
            let ptr = page.direct_map().as_mut_ptr::<u8>();
            core::mem::forget(page); // dlmalloc manages the lifetime
            #[cfg(feature = "heap-sweep")]
            pages::add(ptr as u64);
            (ptr, PAGE_2M as usize, 0)
        } else {
            (core::ptr::null_mut(), 0, 0)
        }
    }

    fn remap(&self, _ptr: *mut u8, _oldsize: usize, _newsize: usize, _can_move: bool) -> *mut u8 {
        core::ptr::null_mut()
    }

    fn free_part(&self, _ptr: *mut u8, _oldsize: usize, _newsize: usize) -> bool {
        false
    }

    fn free(&self, ptr: *mut u8, _size: usize) -> bool {
        #[cfg(feature = "heap-sweep")]
        pages::remove(ptr as u64);
        let phys = ptr as u64 - PHYS_OFFSET;
        drop(pmm::PhysPage::from_raw(phys));
        true
    }

    fn can_release_part(&self, _flags: u32) -> bool {
        false
    }

    fn allocates_zeros(&self) -> bool {
        true
    }

    fn page_size(&self) -> usize {
        PAGE_2M as usize
    }
}

/// Bands of known bytes around every heap allocation, checked at free and, for live allocations, by [`check_live`] and [`sweep`].
#[cfg(feature = "heap-tripwire")]
mod tripwire {
    use core::alloc::Layout;

    /// Bytes in one band record: `OPEN`, size, alignment, `CLOSE`.
    pub const RECORD: usize = 32;

    // Magic words are XORed with the address they're written at, so a copy spilled elsewhere (e.g. to a stack on the same heap) never reads as a record.
    const OPEN: u64 = 0x4845_4144_5a4f_4e45;
    const CLOSE: u64 = 0x5a4f_4e45_4441_4548;

    /// Words written at `record` and 24 bytes above it.
    pub const fn open_word(record: u64) -> u64 { OPEN ^ record }
    pub const fn close_word(record: u64) -> u64 { CLOSE ^ record }

    /// Every band byte that is not the record.
    pub const FILL: u8 = 0x5a;
    pub const FILL_WORD: u64 = u64::from_ne_bytes([FILL; 8]);

    #[cfg(all(feature = "heap-band-notail", feature = "heap-band-nohead"))]
    compile_error!("heap-band-notail and heap-band-nohead are two arms of one sweep; build one");

    #[cfg(not(any(feature = "heap-band-notail", feature = "heap-band-nohead")))]
    mod shape {
        pub const HEAD_MIN: usize = 32;
        pub const TAIL: usize = 32;
    }
    #[cfg(all(feature = "heap-band-notail", not(feature = "heap-band-nohead")))]
    mod shape {
        pub const HEAD_MIN: usize = 32;
        pub const TAIL: usize = 0;
    }
    #[cfg(all(feature = "heap-band-nohead", not(feature = "heap-band-notail")))]
    mod shape {
        pub const HEAD_MIN: usize = 0;
        pub const TAIL: usize = super::RECORD + 32;
    }

    pub use shape::{HEAD_MIN, TAIL};

    /// Where the record sits in the tail band, and how much of it is fill.
    pub const TAIL_FILL_OFF: usize = if HEAD_MIN == 0 { RECORD } else { 0 };
    pub const TAIL_FILL: usize = TAIL - TAIL_FILL_OFF;
    // Band walks step 8 bytes, so this must stay a multiple of 8 for the walk to land exactly on `TAIL_FILL`.
    const _: () = assert!(TAIL_FILL.is_multiple_of(8), "the tail band's fill is walked in words");

    /// Head band width for an allocation of this alignment.
    pub const fn head(align: usize) -> usize {
        if HEAD_MIN == 0 {
            0
        } else if align > HEAD_MIN {
            align
        } else {
            HEAD_MIN
        }
    }

    /// What is actually asked of `dlmalloc`.
    pub fn outer(layout: Layout) -> Layout {
        let size = layout.size() + head(layout.align()) + TAIL;
        // Only fails past `isize::MAX`, already excluded by the caller's `MAX_HEAP_ALLOC` check.
        Layout::from_size_align(size, layout.align()).expect("heap-tripwire: banded layout")
    }

    /// The record's first byte for an allocation whose payload is `payload`.
    ///
    /// # Safety
    /// `payload`/`size` describe an allocation this module armed.
    pub unsafe fn record_ptr(payload: *mut u8, size: usize) -> *mut u8 {
        if HEAD_MIN == 0 { payload.add(size) } else { payload.sub(RECORD) }
    }

    /// Write the bands and hand back the payload; a null base passes through unchanged.
    ///
    /// # Safety
    /// `base` is null, or points at `outer(layout)` bytes the caller owns.
    pub unsafe fn arm(base: *mut u8, layout: Layout) -> *mut u8 {
        if base.is_null() {
            return base;
        }
        let payload = base.add(head(layout.align()));
        core::ptr::write_bytes(base, FILL, head(layout.align()));
        core::ptr::write_bytes(payload.add(layout.size()), FILL, TAIL);
        let record = record_ptr(payload, layout.size());
        record.cast::<u64>().write_unaligned(open_word(record as u64));
        record.add(8).cast::<u64>().write_unaligned(layout.size() as u64);
        record.add(16).cast::<u64>().write_unaligned(layout.align() as u64);
        record.add(24).cast::<u64>().write_unaligned(close_word(record as u64));
        payload
    }

    /// Read the bands back, retire the record and hand back what `dlmalloc` was given.
    ///
    /// Invariant: both magic words must be cleared, or a freed chunk's leftover record reads as a live allocation to [`super::sweep`].
    ///
    /// # Safety
    /// `ptr` and `layout` are a pair this module's [`arm`] produced.
    pub unsafe fn disarm(ptr: *mut u8, layout: Layout) -> (*mut u8, Layout) {
        check(ptr, layout, "dealloc");
        let head = head(layout.align());
        // `while`, not `for`: a zero-width band makes the range empty at compile time, which clippy denies.
        let fill_bytes = head.saturating_sub(RECORD);
        let mut i = 0;
        while i < fill_bytes {
            let byte = ptr.sub(head).add(i).read();
            assert!(
                byte == FILL,
                "HEAP TRIPWIRE (dealloc): {ptr:?} was written {} bytes BELOW its {}-byte \
                 allocation — head band byte +{i} of {fill_bytes} is {byte:#04x}, want \
                 {FILL:#04x}",
                head - i, layout.size(),
            );
            i += 1;
        }
        let record = record_ptr(ptr, layout.size());
        record.cast::<u64>().write_unaligned(0);
        record.add(24).cast::<u64>().write_unaligned(0);
        (ptr.sub(head), outer(layout))
    }

    /// Checks the live edges of an allocation: the record, and the fill band beside it.
    ///
    /// Checks the record only, not the whole head band: in the default shape it sits at the top of the head band, the first thing a stack running off its own bottom reaches.
    ///
    /// # Safety
    /// `ptr` and `layout` are a pair this module's [`arm`] produced, and nothing is freeing that allocation concurrently.
    pub unsafe fn check(ptr: *mut u8, layout: Layout, site: &str) {
        let record = record_ptr(ptr, layout.size());
        let open = record.cast::<u64>().read_unaligned();
        let size = record.add(8).cast::<u64>().read_unaligned();
        let align = record.add(16).cast::<u64>().read_unaligned();
        let close = record.add(24).cast::<u64>().read_unaligned();
        assert!(
            open == open_word(record as u64) && close == close_word(record as u64)
                && size == layout.size() as u64
                && align == layout.align() as u64,
            "HEAP TRIPWIRE ({site}): the record of {ptr:?} is not the one that was written \
             — open {open:#018x}, close {close:#018x}, recorded {size}/{align}, holder says \
             {}/{}. In the default band shape this is the first word a kernel stack running \
             off its own bottom reaches.",
            layout.size(), layout.align(),
        );
        check_tail_fill(ptr, layout.size(), site);
    }

    /// Checks the tail band's fill bytes.
    ///
    /// # Safety
    /// `ptr`/`size` describe an allocation this module armed.
    pub unsafe fn check_tail_fill(ptr: *mut u8, size: usize, site: &str) {
        let fill = ptr.add(size).add(TAIL_FILL_OFF);
        // `!=`, not `<`: with `heap-band-notail` the band can be zero-width, and `< 0` is a clippy denial.
        let mut off = 0;
        while off != TAIL_FILL {
            let word = fill.add(off).cast::<u64>().read_unaligned();
            if word != FILL_WORD {
                let band = band_words(fill);
                panic!(
                    "HEAP TRIPWIRE ({site}): {ptr:?} was written past its {size}-byte \
                     allocation — tail band word +{} is {word:#018x}; the band reads \
                     [{:#018x}, {:#018x}, {:#018x}, {:#018x}] and its first byte stands at \
                     {:#018x}",
                    TAIL_FILL_OFF + off,
                    band[0], band[1], band[2], band[3], fill as u64,
                );
            }
            off += 8;
        }
    }

    /// First four words of a band, for the fire message: one dirty word alone can't be told from a plausible false read.
    ///
    /// # Safety
    /// `fill` is the first byte of a band this module armed.
    unsafe fn band_words(fill: *const u8) -> [u64; 4] {
        let mut out = [FILL_WORD; 4];
        let mut i = 0;
        while i < 4 && (i + 1) * 8 <= TAIL_FILL {
            out[i] = fill.add(i * 8).cast::<u64>().read_unaligned();
            i += 1;
        }
        out
    }
}

/// The tripwire's absent half: every entry point, costing nothing.
#[cfg(not(feature = "heap-tripwire"))]
mod tripwire {
    use core::alloc::Layout;

    #[inline(always)]
    pub fn outer(layout: Layout) -> Layout { layout }

    /// # Safety
    /// Trivially sound: it hands back its argument.
    #[inline(always)]
    pub unsafe fn arm(base: *mut u8, _layout: Layout) -> *mut u8 { base }

    /// # Safety
    /// Trivially sound: it hands back its arguments.
    #[inline(always)]
    pub unsafe fn disarm(ptr: *mut u8, layout: Layout) -> (*mut u8, Layout) { (ptr, layout) }
}

/// Reads the bands of a live heap allocation, before it is freed.
///
/// Gated on the feature rather than a no-op without it: its one caller is gated too.
///
/// # Safety
/// `ptr` and `layout` are a pair `GlobalAlloc::alloc` returned, and nothing is freeing that allocation concurrently.
#[cfg(feature = "heap-tripwire")]
pub unsafe fn check_live(ptr: *mut u8, layout: Layout, site: &str) {
    tripwire::check(ptr, layout, site)
}

/// Reads every live band in the heap, at a point the caller chooses rather than waiting for a free that may never come.
///
/// Invariant: the panic must happen after the lock guarding the walk is dropped, or a panicking sweep abandons the heap with the lock held.
#[cfg(feature = "heap-sweep")]
pub fn sweep(site: &str) {
    SWEPT.0.fetch_add(1, Ordering::Relaxed);
    let outcome = {
        let _held = ALLOCATOR.dlmalloc.lock();
        walk()
    };
    let Some(bad) = outcome else { return };
    let payload = bad.payload as *const u8;
    panic!(
        "HEAP TRIPWIRE (sweep at {site}): {} of the live allocation at {payload:?} \
         ({} bytes, align {}) holds {:#018x} at byte +{}, want {:#018x}. Nothing freed this \
         allocation and nothing may write outside it, so a neighbour of it overran — the \
         dirty bytes are what the writer wrote and the size and alignment are the victim's. \
         Sweeps so far: {}, live bands on this walk: {}.",
        bad.what,
        bad.size,
        bad.align,
        bad.found,
        bad.off,
        bad.want,
        SWEPT.0.load(Ordering::Relaxed),
        SWEPT.1.load(Ordering::Relaxed),
    );
}

/// Holds `dlmalloc`'s lock for `ns` of guest time; touches nothing through it.
#[cfg(feature = "heap-lockspin")]
pub fn hold_lock(ns: u64) {
    let _held = ALLOCATOR.dlmalloc.lock();
    crate::sched::driver::spin_for(ns);
}

/// Sweep count and live records from the last sweep, or `None` without `heap-sweep`.
#[cfg(feature = "heap-sweep")]
pub fn sweep_stats() -> Option<(u64, u64, bool)> {
    Some((
        SWEPT.0.load(Ordering::Relaxed),
        SWEPT.1.load(Ordering::Relaxed),
        pages::OVERFLOWED.load(Ordering::Relaxed),
    ))
}

/// See the `heap-sweep` arm above.
#[cfg(not(feature = "heap-sweep"))]
pub fn sweep_stats() -> Option<(u64, u64, bool)> {
    None
}

/// Sweeps run, and live records the last one walked.
#[cfg(feature = "heap-sweep")]
static SWEPT: (core::sync::atomic::AtomicU64, core::sync::atomic::AtomicU64) =
    (core::sync::atomic::AtomicU64::new(0), core::sync::atomic::AtomicU64::new(0));

/// A band byte that didn't match; plain data because the walk that builds it may not panic.
#[cfg(feature = "heap-sweep")]
#[derive(Clone, Copy)]
struct Bad {
    payload: u64,
    size: u64,
    align: u64,
    what: &'static str,
    off: usize,
    found: u64,
    want: u64,
}

/// Every heap page handed to `dlmalloc`; written and read only under `dlmalloc.lock()`.
#[cfg(feature = "heap-sweep")]
mod pages {
    use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    /// Sized so the walk stays complete for any workload this kernel has.
    const MAX: usize = 512;

    pub static BASES: [AtomicU64; MAX] = [const { AtomicU64::new(0) }; MAX];

    /// Set once the table fills; past that point the sweep covers only part of the heap.
    pub static OVERFLOWED: AtomicBool = AtomicBool::new(false);

    /// Invariant: runs inside `dlmalloc.lock()` and may not panic.
    pub fn add(base: u64) {
        for slot in &BASES {
            if slot.compare_exchange(0, base, Ordering::Relaxed, Ordering::Relaxed).is_ok() {
                return;
            }
        }
        OVERFLOWED.store(true, Ordering::Relaxed);
    }

    /// A page the table never took is simply not there to remove.
    pub fn remove(base: u64) {
        for slot in &BASES {
            if slot.compare_exchange(base, 0, Ordering::Relaxed, Ordering::Relaxed).is_ok() {
                return;
            }
        }
    }
}

/// Walks every heap page for band records; answers rather than asserting. Caller holds `dlmalloc.lock()`.
#[cfg(feature = "heap-sweep")]
fn walk() -> Option<Bad> {
    let mut seen = 0u64;
    let mut bad = None;
    for slot in &pages::BASES {
        let base = slot.load(Ordering::Relaxed);
        if base == 0 {
            continue;
        }
        // SAFETY: `KernelPageSource::free` clears this slot before returning the page, so `base` is still live and mapped — any byte pattern here is valid to read as words.
        let words = unsafe {
            core::slice::from_raw_parts(base as *const u64, PAGE_2M as usize / 8)
        };
        for (i, word) in words.iter().enumerate() {
            let record = base + (i * 8) as u64;
            if *word != tripwire::open_word(record) {
                continue;
            }
            // SAFETY: `record` is a word inside this page; `identify` reads no further than the three words above it before validating.
            let Some(found) = (unsafe { identify(record, base) }) else { continue };
            seen += 1;
            if bad.is_none() {
                // SAFETY: `identify` placed the whole banded allocation inside this page.
                bad = unsafe { inspect(found) };
            }
        }
    }
    SWEPT.1.store(seen, Ordering::Relaxed);
    bad
}

/// A record the walk confirmed, resolved to its allocation.
#[cfg(feature = "heap-sweep")]
#[derive(Clone, Copy)]
struct Found {
    payload: *mut u8,
    size: usize,
    align: usize,
}

/// Checks whether `record` is the start of a real band record and, if so, resolves the allocation it describes.
///
/// A candidate that fails any structural check is skipped, not reported.
///
/// # Safety
/// `record` is a word inside the 2 MiB page at `base`.
#[cfg(feature = "heap-sweep")]
unsafe fn identify(record: u64, base: u64) -> Option<Found> {
    let page = base..base + PAGE_2M;
    if record + tripwire::RECORD as u64 > page.end {
        return None;
    }
    let r = record as *const u64;
    if r.add(3).read_unaligned() != tripwire::close_word(record) {
        return None;
    }
    let size = r.add(1).read_unaligned();
    let align = r.add(2).read_unaligned();
    if size > MAX_HEAP_ALLOC as u64 || !align.is_power_of_two() || align > PAGE_2M {
        return None;
    }
    let (size, align) = (size as usize, align as usize);
    let head = tripwire::head(align);
    let payload = if tripwire::HEAD_MIN == 0 {
        record.checked_sub(size as u64)?
    } else {
        record + tripwire::RECORD as u64
    };
    if payload % align as u64 != 0 {
        return None;
    }
    let bottom = payload.checked_sub(head as u64)?;
    let top = payload.checked_add((size + tripwire::TAIL) as u64)?;
    if bottom < page.start || top > page.end {
        return None;
    }
    Some(Found { payload: payload as *mut u8, size, align })
}

/// The bands of one identified allocation.
///
/// # Safety
/// `found` came from [`identify`], which placed its bands inside a mapped page.
#[cfg(feature = "heap-sweep")]
unsafe fn inspect(found: Found) -> Option<Bad> {
    let Found { payload, size, align } = found;
    let bad = |what, off, word| {
        Some(Bad {
            payload: payload as u64,
            size: size as u64,
            align: align as u64,
            what,
            off,
            found: word,
            want: tripwire::FILL_WORD,
        })
    };
    // Checks the tail band first, then the head band.
    let tail = payload.add(size).add(tripwire::TAIL_FILL_OFF);
    let mut off = 0;
    while off != tripwire::TAIL_FILL {
        let word = tail.add(off).cast::<u64>().read_unaligned();
        if word != tripwire::FILL_WORD {
            return bad("the tail band", tripwire::TAIL_FILL_OFF + off, word);
        }
        off += 8;
    }
    let head = tripwire::head(align);
    let bottom = payload.sub(head);
    let fill_bytes = head.saturating_sub(tripwire::RECORD);
    let mut off = 0;
    while off < fill_bytes {
        let word = bottom.add(off).cast::<u64>().read_unaligned();
        if word != tripwire::FILL_WORD {
            return bad("the head band", off, word);
        }
        off += 8;
    }
    None
}

struct KernelAllocator {
    dlmalloc: Lock<dlmalloc::Dlmalloc<KernelPageSource>>,
    phase: AtomicU8,
}

const PHASE_UNINIT: u8 = 0;
const PHASE_EARLY: u8 = 1;
const PHASE_READY: u8 = 2;

use crate::sync::Lock;

impl KernelAllocator {
    const fn new() -> Self {
        Self {
            dlmalloc: Lock::new(dlmalloc::Dlmalloc::new_with_allocator(KernelPageSource)),
            phase: AtomicU8::new(PHASE_UNINIT),
        }
    }
}

// A null returned from `alloc` is `alloc`'s no_std default error handler's business, and it panics naming the size, the layer and the call site — which is why no `#[alloc_error_handler]` is installed anywhere in this kernel.
// SAFETY: an early-phase pointer is recognized on `dealloc` by address range (`is_early_ptr`), not by re-reading the phase, so it frees correctly even after `init()` switches to PHASE_READY.
unsafe impl GlobalAlloc for KernelAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        match self.phase.load(Ordering::Acquire) {
            PHASE_UNINIT => core::ptr::null_mut(),
            PHASE_EARLY => early_alloc(layout),
            PHASE_READY => {
                assert!(layout.align() < PAGE_2M as usize,
                    "GlobalAlloc: {:#x} bytes with {:#x} align — use PageAlloc", layout.size(), layout.align());
                // Checked before the lock: a panic inside `dlmalloc.lock()` would abandon the heap with the lock held.
                assert!(layout.size() <= MAX_HEAP_ALLOC,
                    "GlobalAlloc: {} bytes exceeds MAX_HEAP_ALLOC ({}) — a caller is using alloc for page-scale memory",
                    layout.size(), MAX_HEAP_ALLOC);
                // Bands are armed after the lock drops, so a failing band never fires inside `dlmalloc.lock()`.
                let outer = tripwire::outer(layout);
                let base = {
                    let mut dlm = self.dlmalloc.lock();
                    dlm.malloc(outer.size(), outer.align())
                };
                tripwire::arm(base, layout)
            }
            // Any byte other than the three phases is corrupt state — fail loudly rather than serve it as READY.
            other => panic!("KernelAllocator: corrupt phase byte {other:#x}"),
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if is_early_ptr(ptr) { return; }
        let (base, outer) = tripwire::disarm(ptr, layout);
        let mut dlm = self.dlmalloc.lock();
        dlm.free(base, outer.size(), outer.align());
    }
}

#[global_allocator]
static ALLOCATOR: KernelAllocator = KernelAllocator::new();

const EARLY_SIZE: usize = 512 * 1024;

#[repr(C, align(4096))]
struct EarlyBuffer([u8; EARLY_SIZE]);

static mut EARLY_BUF: EarlyBuffer = EarlyBuffer([0; EARLY_SIZE]);
static mut EARLY_POS: usize = 0;

unsafe fn early_alloc(layout: Layout) -> *mut u8 {
    let buf = core::ptr::addr_of_mut!(EARLY_BUF) as *mut u8;
    let aligned = (EARLY_POS + layout.align() - 1) & !(layout.align() - 1);
    let new_pos = aligned + layout.size();
    if new_pos > EARLY_SIZE {
        return core::ptr::null_mut();
    }
    EARLY_POS = new_pos;
    buf.add(aligned)
}

fn is_early_ptr(ptr: *mut u8) -> bool {
    let buf_start = core::ptr::addr_of!(EARLY_BUF) as usize;
    let p = ptr as usize;
    p >= buf_start && p < buf_start + EARLY_SIZE
}

/// Phase 1: Enable early bump allocator (before paging).
pub(super) fn init_early() {
    ALLOCATOR.phase.store(PHASE_EARLY, Ordering::Release);
}

/// Phase 2: Switch to dlmalloc (after pmm + paging are ready).
pub(super) fn init() {
    ALLOCATOR.phase.store(PHASE_READY, Ordering::Release);
}
