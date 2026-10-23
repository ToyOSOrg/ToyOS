//! Physical memory: `kernel::frames`' allocator over every usable frame
//! firmware's map names, its records laid out at boot in memory it manages.
//!
//! [`Frame`] owns one 4 KiB frame and [`PhysPage`] one 2 MiB superframe; each
//! returns its memory when it drops. Pins ([`pin_range`]) are taken without
//! the allocator's lock.
//!
//! What a process causes — a commitment, a page table, a pipe's ring, a run a
//! device is lent, a [`charge`] of the kernel heap — is refused rather than
//! take what the allocator holds back for the kernel; only the kernel's own
//! allocations may take it.

use core::sync::atomic::{AtomicPtr, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use kernel::frames::{self, Frames, Pins, Super, Unpinned, FRAME, SUPER};
pub use kernel::frames::Discarded;

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
    phys: u64,
}

impl PhysPage {
    /// Access this page through the kernel direct map.
    pub fn direct_map(&self) -> DirectMap {
        DirectMap::from_phys(self.phys)
    }
}

impl Drop for PhysPage {
    fn drop(&mut self) {
        FRAMES.lock().as_mut().expect("pmm: a page outlived no allocator").free_super(self.phys, &pins());
    }
}

/// Owns one 4 KiB frame, zeroed when it was allocated; dropping it returns
/// the frame.
pub struct Frame {
    phys: u64,
}

impl Frame {

    pub fn direct_map(&self) -> DirectMap {
        DirectMap::from_phys(self.phys)
    }

    /// Give the frame's ownership to whatever records `phys`, which must
    /// return it through [`Frame::from_raw`].
    pub fn into_raw(self) -> u64 {
        core::mem::ManuallyDrop::new(self).phys
    }

    /// # Safety
    /// `phys` is a frame [`Frame::into_raw`] gave up, taken back once.
    pub unsafe fn from_raw(phys: u64) -> Self {
        Self { phys }
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        FRAMES.lock().as_mut().expect("pmm: a frame outlived no allocator").free_frame(self.phys, &pins());
    }
}

static FRAMES: Lock<Option<Frames<'static>>> = Lock::new(None);

/// Give back `frames`, which leaves the space owned, under one hold of the
/// allocator's lock.
///
/// # Safety
/// Each is a frame [`Frame::into_raw`] gave up, that no leaf on any CPU maps
/// any more and nothing else frees.
pub unsafe fn free_raw(frames: &[u64]) {
    let pins = pins();
    let mut guard = FRAMES.lock();
    let allocator = guard.as_mut().expect("pmm: a frame outlived no allocator");
    for &phys in frames {
        allocator.free_frame(phys, &pins);
    }
}

/// Give back every frame of `frames` under one hold of the allocator's lock.
pub fn free_all(frames: alloc::boxed::Box<[Frame]>) {
    let mut frames = frames.into_vec();
    {
        let pins = pins();
        let mut guard = FRAMES.lock();
        let allocator = guard.as_mut().expect("pmm: a frame outlived no allocator");
        for frame in frames.drain(..) {
            allocator.free_frame(frame.into_raw(), &pins);
        }
    }
    // The list goes back to the heap only once the allocator's lock is let
    // go, since the heap may give a page back through it.
    drop(frames);
}
/// The pin counters, published once by [`init`]: where they are, how many,
/// and the physical address the first one counts for.
static PINS: (AtomicPtr<AtomicU32>, AtomicUsize, AtomicU64) =
    (AtomicPtr::new(core::ptr::null_mut()), AtomicUsize::new(0), AtomicU64::new(0));

fn pins() -> Pins<'static> {
    let at = PINS.0.load(Ordering::Acquire);
    if at.is_null() {
        return Pins::new(0, &[]);
    }
    // SAFETY: `init` stored a `'static` slice's pointer before its length and
    // base, and the counters are never freed or moved.
    let words = unsafe { core::slice::from_raw_parts(at, PINS.1.load(Ordering::Acquire)) };
    Pins::new(PINS.2.load(Ordering::Acquire), words)
}

/// Describe every usable frame of `entries` but `reserved`, and lay the
/// allocator's records out of one stretch of that memory the boot map reaches.
pub(super) fn init(entries: &[MemoryMapEntry], reserved: &[Region]) {
    let usable = || entries.iter().filter(|e| is_usable(e));
    let lo = usable().map(|e| e.start & !(SUPER - 1)).min();
    let hi = usable().map(|e| e.end.next_multiple_of(SUPER)).max();
    let (Some(base), Some(top)) = (lo, hi) else { return };
    let supers = frames::supers_for(base, top);
    let pins = frames::pins_for(supers);
    let records = (supers * core::mem::size_of::<Super>()).next_multiple_of(core::mem::align_of::<AtomicU32>());
    let bytes = (records + pins * core::mem::size_of::<AtomicU32>()) as u64;
    let at = place(entries, reserved, bytes).unwrap_or_else(|| {
        panic!("pmm: no {bytes} usable bytes below {:#x} outside every reserved region for the allocator's records", toyos_bootmap::BOOT_MAP_BYTES)
    });
    let start = DirectMap::from_phys(at).as_mut_ptr::<u8>();
    // SAFETY: `place` found `[at, at + bytes)` usable, outside every reserved
    // region and inside the boot map, and it is withheld from the allocator
    // below, so nothing else ever reaches it; zeroed whole, so every pin
    // counter starts at zero and the records are overwritten by `Frames::new`.
    let (records, words) = unsafe {
        core::ptr::write_bytes(start, 0, bytes as usize);
        (
            core::slice::from_raw_parts_mut(start.cast::<Super>(), supers),
            core::slice::from_raw_parts(start.add(records).cast::<AtomicU32>(), pins),
        )
    };

    let mut frames = Frames::new(base, records);
    let mut firmware_bytes = 0u64;
    let mut usable_entries = 0u64;
    for entry in usable() {
        usable_entries += 1;
        firmware_bytes += entry.end - entry.start;
        frames.add(entry.start, entry.end);
    }
    let described = frames.usable_frames();
    // Frame 0 too: physical address zero is what every device table and
    // register that names nothing holds.
    let own = [Region { start: at, end: at + bytes }, Region { start: 0, end: FRAME }];
    for region in reserved.iter().chain(&own) {
        frames.withhold(region.start, region.end);
    }
    frames.seal();
    frames.keep(kept_supers(&frames));
    let managed = frames.usable_frames() * FRAME;
    let withheld = (described - frames.usable_frames()) * FRAME;

    PINS.2.store(base, Ordering::Release);
    PINS.1.store(pins, Ordering::Release);
    PINS.0.store(words.as_ptr().cast_mut(), Ordering::Release);
    *FRAMES.lock() = Some(frames);

    // **Exact, and it has to be**: every byte the firmware called usable is a
    // frame this allocator manages, a frame withheld because a reserved region
    // or the allocator's own records touch it, or a fragment of an entry that
    // is not whole frames. Saturating, because the map is firmware's and
    // overlapping usable entries make the accounted total exceed its own: a
    // zero then is a sum that does not balance, which `pmm_accounting` reads.
    crate::log!(
        "pmm: the firmware map calls {firmware_bytes} bytes usable in {usable_entries} entries; \
         managed={managed} withheld={withheld} unaligned={}, and the three sum to it; \
         frames={} superframes={supers} records={bytes} at {at:#x} base={base:#x}",
        firmware_bytes.saturating_sub(managed).saturating_sub(withheld),
        managed / FRAME,
    );
}

/// The lowest frame-aligned `bytes` of usable memory below the boot map's
/// reach that no reserved region touches.
fn place(entries: &[MemoryMapEntry], reserved: &[Region], bytes: u64) -> Option<u64> {
    let reach = toyos_bootmap::BOOT_MAP_BYTES;
    let lowest_in = |entry: &MemoryMapEntry| {
        let mut at = entry.start.next_multiple_of(FRAME);
        while at.checked_add(bytes)? <= entry.end.min(reach) {
            match reserved.iter().find(|r| at < r.end && r.start < at + bytes) {
                Some(r) => at = r.end.next_multiple_of(FRAME),
                None => return Some(at),
            }
        }
        None
    };
    entries.iter().filter(|e| is_usable(e)).filter_map(lowest_in).min()
}

/// The whole superframes nothing a process causes may take, so that the
/// kernel's own allocations, its heap's growth past its charges among them,
/// meet room when processes have taken the rest: 1/256 of managed memory,
/// and 16 MiB at least.
fn kept_supers(frames: &Frames) -> u64 {
    (frames.usable_frames() / 256).max((16 << 20) / FRAME).div_ceil(SUPER / FRAME)
}

/// Promise `frames` to a commitment, which draws each with
/// [`alloc_promised`] when it is first touched; `false`, and nothing
/// promised, where that would take what the kernel holds back.
pub fn promise(frames: u64) -> bool {
    FRAMES.lock().as_mut().is_some_and(|f| f.promise(frames))
}

/// Charge `bytes` of the kernel heap to what a process caused, before it is
/// allocated; `false`, and nothing charged, where covering them would take
/// what the kernel holds back. The heap grows into what charges reserve
/// first, so an allocation a charge covers finds its room.
pub fn charge(bytes: u64) -> bool {
    FRAMES.lock().as_mut().is_some_and(|f| f.charge(bytes))
}

/// Give back a [`charge`].
pub fn uncharge(bytes: u64) {
    FRAMES.lock().as_mut().expect("pmm: a charge outlived no allocator").uncharge(bytes);
}

/// Take back promises no commitment holds any more.
pub fn unpromise(frames: u64) {
    if frames > 0 {
        FRAMES.lock().as_mut().expect("pmm: a promise outlived no allocator").unpromise(frames);
    }
}

/// [`Frames::discard`] of `phys`, a committed page's frame the caller holds
/// the lock of the mapping space for.
///
/// # Safety
/// `phys` is an owned leaf's frame, which no leaf on any CPU maps any more,
/// unless this answers [`Discarded::Kept`].
pub unsafe fn discard(phys: u64) -> Discarded {
    FRAMES.lock().as_mut().expect("pmm: a frame outlived no allocator").discard(phys, &pins())
}

/// A zeroed frame a promise held, which the promise guarantees is free.
pub fn alloc_promised() -> Frame {
    let phys = FRAMES.lock().as_mut().expect("pmm: a promise outlived no allocator").alloc_promised();
    let frame = Frame { phys };
    // SAFETY: just taken off the allocator, so unaliased, and inside the direct map.
    unsafe { core::ptr::write_bytes(frame.direct_map().as_mut_ptr::<u8>(), 0, FRAME as usize) };
    frame
}

/// A zeroed frame for a page table a process causes, refused rather than
/// take what the kernel holds back: a process's tables are not promised.
pub fn alloc_table_frame() -> Option<Frame> {
    let phys = FRAMES.lock().as_mut()?.alloc_frame_for_process()?;
    let frame = Frame { phys };
    // SAFETY: just taken off the allocator, so unaliased, and inside the direct map.
    unsafe { core::ptr::write_bytes(frame.direct_map().as_mut_ptr::<u8>(), 0, FRAME as usize) };
    Some(frame)
}

/// One zeroed 2 MiB page for the kernel's own use.
pub fn alloc_page() -> Option<PhysPage> {
    let phys = FRAMES.lock().as_mut()?.alloc_super()?;
    Some(zeroed(PhysPage { phys }))
}

/// One zeroed 2 MiB page a process causes, such as a pipe's ring: refused
/// rather than take what the kernel holds back.
pub fn alloc_page_for_process() -> Option<PhysPage> {
    let phys = FRAMES.lock().as_mut()?.alloc_run_for_process(1)?;
    Some(zeroed(PhysPage { phys }))
}

fn zeroed(page: PhysPage) -> PhysPage {
    // SAFETY: just taken off the allocator, so unaliased, and the direct map covers every address it can name.
    unsafe { core::ptr::write_bytes(page.direct_map().as_mut_ptr::<u8>(), 0, PAGE_2M as usize) };
    page
}

/// A 2 MiB superframe the kernel heap holds, holding what its last owner left
/// in it: the heap answers uninitialized memory, and `alloc_zeroed` writes its
/// own zeros. It goes back when it drops.
pub(super) struct HeapPage {
    phys: u64,
}

impl HeapPage {
    /// Grow the heap by one superframe: one its charges reserved, or one for
    /// the kernel's own use. Does not heap-allocate: the heap calls it when
    /// it is full.
    pub(super) fn grow() -> Option<Self> {
        let phys = FRAMES.lock().as_mut()?.alloc_heap()?;
        Some(Self { phys })
    }

    pub(super) fn direct_map(&self) -> DirectMap {
        DirectMap::from_phys(self.phys)
    }

    /// The page the heap gave up with [`core::mem::forget`] at `phys`, taken
    /// back once.
    pub(super) fn from_raw(phys: u64) -> Self {
        Self { phys }
    }
}

impl Drop for HeapPage {
    fn drop(&mut self) {
        FRAMES.lock().as_mut().expect("pmm: a heap page outlived no allocator").free_heap(self.phys, &pins());
    }
}

/// One zeroed 4 KiB frame.
pub fn alloc_frame() -> Option<Frame> {
    let phys = FRAMES.lock().as_mut()?.alloc_frame()?;
    let frame = Frame { phys };
    // SAFETY: just taken off the allocator, so unaliased, and inside the direct map.
    unsafe { core::ptr::write_bytes(frame.direct_map().as_mut_ptr::<u8>(), 0, FRAME as usize) };
    Some(frame)
}

/// `count` physically contiguous zeroed 2 MiB pages a process causes, such as
/// memory a device is lent: refused rather than take what the kernel holds
/// back.
pub fn alloc_contiguous(count: usize) -> Option<alloc::vec::Vec<PhysPage>> {
    // `count` comes from userland, so a bogus 0 and a legitimate `mmap(0)` can't be told apart here — refuse, don't assert.
    if count == 0 {
        return None;
    }
    let base = FRAMES.lock().as_mut()?.alloc_run_for_process(count)?;
    Some(run(base, count))
}

/// `count` physically contiguous zeroed 2 MiB pages for the kernel's own use,
/// such as a kernel driver's DMA pool.
pub fn alloc_contiguous_for_kernel(count: usize) -> Option<alloc::vec::Vec<PhysPage>> {
    assert!(count > 0, "pmm: a kernel run of no pages");
    let base = FRAMES.lock().as_mut()?.alloc_run(count)?;
    Some(run(base, count))
}

/// The pages of a run just taken at `base`, zeroed.
fn run(base: u64, count: usize) -> alloc::vec::Vec<PhysPage> {
    let mut pages = alloc::vec::Vec::with_capacity(count);
    for i in 0..count as u64 {
        let page = PhysPage { phys: base + i * PAGE_2M };
        // SAFETY: every superframe of the run was just taken off the allocator, so it is unaliased and within the direct map.
        unsafe { core::ptr::write_bytes(page.direct_map().as_mut_ptr::<u8>(), 0, PAGE_2M as usize) };
        pages.push(page);
    }
    pages
}

/// Pin every managed frame `[phys, phys + len)` touches so none is reissued while a user-copy window covers it; `false`, and nothing pinned, on a pin-count overflow.
pub fn pin_range(phys: u64, len: usize) -> bool {
    let pins = pins();
    let frames = frame_span(phys, len);
    for (n, frame) in frames.clone().enumerate() {
        if !pins.pin(frame) {
            frames.take(n).for_each(|undo| unpin(&pins, undo));
            return false;
        }
    }
    true
}

/// Release a pin [`pin_range`] took over the same range.
pub fn unpin_range(phys: u64, len: usize) {
    let pins = pins();
    frame_span(phys, len).for_each(|frame| unpin(&pins, frame));
}

fn unpin(pins: &Pins, frame: u64) {
    if pins.unpin(frame) == Unpinned::Release {
        FRAMES.lock().as_mut().expect("pmm: a pinned frame outlived no allocator").release(frame);
    }
}

/// Every frame `[phys, phys + len)` touches.
fn frame_span(phys: u64, len: usize) -> impl Iterator<Item = u64> + Clone {
    let end = if len == 0 { phys & !(FRAME - 1) } else { phys + len as u64 };
    ((phys & !(FRAME - 1))..end).step_by(FRAME as usize)
}

/// One past the highest physical frame this kernel manages; the IOMMU identity domain's exclusive upper bound.
/// Taken from the allocator, not the firmware memory map, whose backing buffer is ordinary free RAM by the time anything asks.
pub fn top() -> u64 {
    FRAMES.lock().as_ref().map_or(0, Frames::top)
}

/// Managed bytes, and those in use or promised.
pub fn stats() -> (u64, u64) {
    let guard = FRAMES.lock();
    let Some(frames) = guard.as_ref() else { return (0, 0) };
    let used = frames.usable_frames() - frames.free_frames() + frames.promised();
    (frames.usable_frames() * FRAME, used * FRAME)
}

fn is_usable(entry: &MemoryMapEntry) -> bool {
    toyos_bootmap::is_usable_type(entry.uefi_type)
}
