//! Physical memory: `kernel::frames`' allocator over every usable frame
//! firmware's map names, its records laid out at boot in memory it manages.
//!
//! [`Frame`] owns one 4 KiB frame, [`PhysPage`] one 2 MiB superframe of the
//! kernel's and [`Run`] a run of an account's; each returns its memory when it
//! drops. Pins ([`pin_range`]) are taken without the allocator's lock.
//!
//! **What a process causes is counted to the job that caused it**, its
//! account — a commitment, a page table, a pipe's bytes, a run a device is
//! lent, a [`charge`] of the kernel heap — and is refused rather than take
//! what the allocator holds back for the kernel or for another account's
//! floor ([`set_floor`]); only the kernel's own allocations may take what it
//! keeps. A job refused memory can wait for it to come back: [`ready`]
//! answers whether it has, and [`returned`] is posted once it has.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicPtr, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use kernel::frames::{self, Frames, Pins, PoolSplit, Super, Unpinned, FRAME, SUPER};
use kernel::sched::job::Job;
pub use kernel::frames::Discarded;

use super::{DirectMap, PAGE_2M};
use crate::sync::Lock;
use crate::watch::IrqWatch;
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
        with(|f| f.free_super(self.phys, &pins()));
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

    /// Give the frame's ownership to whatever records `phys`: a page
    /// committed to a job, which returns it through [`free_drawn`].
    pub fn into_raw(self) -> u64 {
        core::mem::ManuallyDrop::new(self).phys
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        with(|f| f.free_frame(self.phys, &pins()));
    }
}

static FRAMES: Lock<Option<Frames<'static>>> = Lock::new(None);

/// Posted once memory a refused job waits for has come back.
static RETURNED: IrqWatch = IrqWatch::new();

/// Run `f` on the allocator, and post [`RETURNED`] once its lock is let go
/// if what `f` gave back met a waiter's want. The post frees nothing, so it
/// may be made wherever memory is given back.
fn with<R>(f: impl FnOnce(&mut Frames<'static>) -> R) -> R {
    try_with(|frames| Some(f(frames))).expect("pmm: memory reached before the allocator")
}

/// [`with`], answering `None` before the allocator exists.
fn try_with<R>(f: impl FnOnce(&mut Frames<'static>) -> Option<R>) -> Option<R> {
    let (answer, returned) = {
        let mut guard = FRAMES.lock();
        let frames = guard.as_mut()?;
        let answer = f(frames);
        (answer, frames.take_returned())
    };
    if returned {
        RETURNED.post_in_place();
    }
    answer
}

/// What a job waiting for memory to come back registers on.
pub fn returned() -> &'static IrqWatch {
    &RETURNED
}

/// Whether what `job` lacked when it was last refused is free for it now;
/// if not, [`returned`] is posted once it is.
pub fn ready(job: &Job) -> bool {
    with(|f| f.ready(job.memory()))
}

/// Hold `bytes` back for `job`, which has none, however short every other
/// job runs: `AlreadyExists` where it has one, and `ResourceExhausted`,
/// nothing changed, where what is free now cannot hold it.
pub fn hold_floor(job: &Job, bytes: u64) -> Result<(), toyos_abi::syscall::SyscallError> {
    use toyos_abi::syscall::SyscallError;
    with(|f| match job.memory().usage().floor {
        0 if f.set_floor(job.memory(), bytes.div_ceil(FRAME)) => Ok(()),
        0 => Err(SyscallError::ResourceExhausted),
        _ => Err(SyscallError::AlreadyExists),
    })
}

/// Let go of what [`hold_floor`] held back for `job`.
pub fn release_floor(job: &Job) {
    let set = with(|f| f.set_floor(job.memory(), 0));
    assert!(set, "pmm: a floor let go of was refused");
}

/// Give back `frames` drawn for pages `job` committed and no longer does,
/// under one hold of the allocator's lock.
///
/// # Safety
/// Each is a frame [`Frame::into_raw`] gave up, that no leaf on any CPU maps
/// any more and nothing else frees.
pub unsafe fn free_drawn(job: &Job, frames: impl IntoIterator<Item = u64>) {
    let pins = pins();
    with(|f| f.free_drawn(job.memory(), frames, &pins));
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

/// Promise `frames` to a commitment of `job`'s, which draws each with
/// [`alloc_promised`] when it is first touched; `false`, and nothing
/// promised, where that would take what is held back.
pub fn promise(job: &Job, frames: u64) -> bool {
    frames == 0 || with(|f| f.promise(job.memory(), frames))
}

/// Charge `bytes` more of the kernel heap to `job`, into `held`, before they
/// are allocated; `false`, and nothing charged, where covering them would
/// take what is held back. The heap grows into what charges reserve first, so
/// an allocation a charge covers finds its room.
pub fn charge(job: &Job, held: &mut PoolSplit, bytes: u64) -> bool {
    with(|f| f.grow(job.memory(), held, bytes))
}

/// Give back `bytes` of a [`charge`].
pub fn uncharge(job: &Job, held: &mut PoolSplit, bytes: u64) {
    with(|f| f.shrink(job.memory(), held, bytes));
}

/// Take back promises none of `job`'s commitments holds any more.
pub fn unpromise(job: &Job, frames: u64) {
    if frames > 0 {
        with(|f| f.unpromise(job.memory(), frames));
    }
}

/// [`Frames::discard`] of `phys`, a committed page's frame the caller holds
/// the lock of the mapping space for.
///
/// # Safety
/// `phys` is an owned leaf's frame, which no leaf on any CPU maps any more,
/// unless this answers [`Discarded::Kept`].
pub unsafe fn discard(phys: u64) -> Discarded {
    let pins = pins();
    with(|f| f.discard(phys, &pins))
}

/// A zeroed frame a promise held, which the promise guarantees is free.
pub fn alloc_promised() -> Frame {
    let frame = Frame { phys: with(Frames::alloc_promised) };
    // SAFETY: just taken off the allocator, so unaliased, and inside the direct map.
    unsafe { core::ptr::write_bytes(frame.direct_map().as_mut_ptr::<u8>(), 0, FRAME as usize) };
    frame
}

/// A zeroed frame for a page table of `job`'s, refused rather than take what
/// is held back: a process's tables are not promised. It goes back through
/// [`free_held`].
pub fn alloc_table_frame(job: &Job) -> Option<u64> {
    let mut phys = 0;
    if !with(|f| f.alloc_held(job.memory(), 1, |frame| phys = frame)) {
        return None;
    }
    // SAFETY: just taken off the allocator, so unaliased, and inside the direct map.
    unsafe { core::ptr::write_bytes(DirectMap::from_phys(phys).as_mut_ptr::<u8>(), 0, FRAME as usize) };
    Some(phys)
}

/// `count` frames for kernel memory of `job`'s that no process maps and that
/// is written before it is read, such as a pipe's bytes, each handed to
/// `each` under one hold of the allocator's lock: refused whole rather than
/// take what is held back. They hold what their last owner left, and go back
/// through [`free_held`].
pub fn alloc_held(job: &Job, count: usize, each: impl FnMut(u64)) -> bool {
    count == 0 || with(|f| f.alloc_held(job.memory(), count as u64, each))
}

/// Give back `frames` [`alloc_table_frame`] or [`alloc_held`] took for
/// `job`, under one hold of the allocator's lock; giving back none takes no
/// lock.
///
/// # Safety
/// Nothing reaches any of them any more: no table links it, no CPU can still
/// walk into it, and nothing else frees it.
pub unsafe fn free_held<I: ExactSizeIterator<Item = u64>>(job: &Job, frames: impl IntoIterator<IntoIter = I>) {
    let frames = frames.into_iter();
    if frames.len() == 0 {
        return;
    }
    let pins = pins();
    with(|f| f.free_held(job.memory(), frames, &pins));
}

/// One zeroed 2 MiB page for the kernel's own use.
pub fn alloc_page() -> Option<PhysPage> {
    let phys = with(Frames::alloc_super)?;
    Some(zeroed(PhysPage { phys }))
}

fn zeroed(page: PhysPage) -> PhysPage {
    // SAFETY: just taken off the allocator, so unaliased, and the direct map covers every address it can name.
    unsafe { core::ptr::write_bytes(page.direct_map().as_mut_ptr::<u8>(), 0, PAGE_2M as usize) };
    page
}

/// Physically contiguous 2 MiB pages a job holds, such as memory a device is
/// lent; they go back, counted to it, when this drops.
pub struct Run {
    base: u64,
    count: usize,
    job: Arc<Job>,
}

impl Run {
    /// `count` zeroed pages in a row for `job`, refused rather than take what
    /// is held back; `None` for none, which a caller's bogus size and a
    /// legitimate zero would both ask.
    pub fn new(job: &Arc<Job>, count: usize) -> Option<Self> {
        if count == 0 {
            return None;
        }
        let base = with(|f| f.alloc_run_for(job.memory(), count))?;
        // SAFETY: every superframe of the run was just taken off the allocator,
        // so it is unaliased and within the direct map.
        unsafe { core::ptr::write_bytes(DirectMap::from_phys(base).as_mut_ptr::<u8>(), 0, count * PAGE_2M as usize) };
        Some(Self { base, count, job: Arc::clone(job) })
    }

    pub fn direct_map(&self) -> DirectMap {
        DirectMap::from_phys(self.base)
    }

    /// Its pages.
    pub fn count(&self) -> usize {
        self.count
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        let pins = pins();
        with(|f| f.free_run_for(self.job.memory(), self.base, self.count, &pins));
    }
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
        with(|f| f.free_heap(self.phys, &pins()));
    }
}

/// One zeroed 4 KiB frame.
pub fn alloc_frame() -> Option<Frame> {
    // Through `try_with`: a frame of a kept superframe frees the rest of it
    // to the room, which may be what a waiter lacks.
    let phys = try_with(Frames::alloc_frame)?;
    let frame = Frame { phys };
    // SAFETY: just taken off the allocator, so unaliased, and inside the direct map.
    unsafe { core::ptr::write_bytes(frame.direct_map().as_mut_ptr::<u8>(), 0, FRAME as usize) };
    Some(frame)
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

/// Whether a pin holds the frame at `phys`.
pub fn pinned(phys: u64) -> bool {
    pins().pinned(phys)
}

/// Release a pin [`pin_range`] took over the same range.
pub fn unpin_range(phys: u64, len: usize) {
    let pins = pins();
    frame_span(phys, len).for_each(|frame| unpin(&pins, frame));
}

fn unpin(pins: &Pins, frame: u64) {
    if pins.unpin(frame) == Unpinned::Release {
        with(|f| f.release(frame));
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
