//! A memory object: bytes more than one address space can map — a region
//! `SYS_SHM_CREATE` makes, a device's grant, a program's TLS block, an inbox's
//! ring, the clock page, a boot image the loader handed over, part of another
//! object's pages — or an aperture over memory the kernel does not own: a
//! device's registers, firmware's framebuffer.
//!
//! **A mapping holds the object, and a handle holds nothing of a mapping**:
//! the object's frames go back when the last of its handles and mappings has.
//! An aperture is the exception that keeps a list of where it is mapped, since
//! closing its last handle is how a device's window is taken back from every
//! process that mapped it.
//!
//! **A fault never allocates.** A region's pages are committed, each a
//! frame of its own, zeroed, counted to the object's job, at the call that
//! commits them: `SYS_SHM_CREATE`, all or a first part of them, then
//! `SYS_SHM_COMMIT`, which any holder that may map it calls. A committed
//! page is the object's to its end. A fault on a page not committed is
//! refused as an access outside every range is, and a copy reads it as
//! zeros.

use alloc::boxed::Box;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use kernel::sched::job::Job;

use toyos_abi::syscall::SyscallError;

use crate::mm::charge::{self, Charge, Exhausted};
use crate::mm::policy::CachePolicy;
use crate::mm::pmm::{self, Run};
use crate::mm::space::Space;
use crate::mm::{PAGE_2M, PAGE_SIZE};
use crate::{DirectMap, UserAddr};

use super::{Held, KObjectVariant, ObjectCore, ZeroHandles};

// What a device-lendable region is whole pages of, as the ABI says.
const _: () = assert!(toyos_abi::syscall::SHM_DEVICE_PAGE == PAGE_2M);

/// Frames in one piece of a [`Backing::Frames`]: 256 MiB of them, whose list
/// is 512 KiB.
const CHUNK: usize = 1 << 16;

/// What an object's bytes are.
enum Backing {
    /// Frames of its own, one per page, in no order, [`CHUNK`] to a piece so
    /// that no piece is past one heap allocation: each word its page's
    /// frame, or 0 while the page is not committed, since no frame is at 0.
    Frames(Vec<Box<[AtomicU64]>>),
    /// 2 MiB pages of its own in one physical run: what a device may be lent
    /// or granted, and what a kernel-built image addresses as one slice.
    Run(Run),
    /// A payload the loader verified and handed over at `phys`, `len` bytes
    /// of the object's pages: kept for the machine's life and written by
    /// nobody, so every mapping of it is read-only (`crate::bootimage`).
    Boot { phys: u64, len: u64 },
    /// Memory the kernel does not own, at `phys`, with where it is mapped
    /// and that list's charge.
    Aperture { phys: u64, mapped: Held<(Vec<Mapped>, Charge)> },
    /// The pages of `whole` from `offset`, which is never itself a `Sub` or
    /// an aperture; `len` its bytes, the payload where `whole` is immutable.
    Sub { whole: Arc<SharedMemObject>, offset: u64, len: u64 },
}

/// A space an aperture is mapped in, and where.
type Mapped = (Weak<Space>, UserAddr, u64);

pub struct SharedMemObject {
    pub(super) core: ObjectCore,
    backing: Backing,
    size: u64,
    cache: CachePolicy,
}

impl SharedMemObject {
    /// Over `core`, charged with what `backing` keeps on the heap.
    fn new(core: ObjectCore, backing: Backing, size: u64, cache: CachePolicy) -> Arc<Self> {
        Arc::new(Self { core, backing, size, cache })
    }

    /// `size` bytes of zeroed memory in whole pages, frame by frame, counted
    /// to `job`; `InvalidArgument` for no bytes or a size no page count
    /// holds, `ResourceExhausted` where frames run out.
    pub fn create(job: &Arc<Job>, size: u64) -> Result<Arc<Self>, SyscallError> {
        Self::reserve(job, size, size)
    }

    /// [`Self::create`], with only its first `committed` bytes' pages
    /// committed; [`Self::commit`] commits the rest.
    pub fn reserve(job: &Arc<Job>, size: u64, committed: u64) -> Result<Arc<Self>, SyscallError> {
        let pages = pages(size)?;
        if committed > size {
            return Err(SyscallError::InvalidArgument);
        }
        let (count, full, rest) = (pages.div_ceil(CHUNK as u64), pages / CHUNK as u64, (pages % CHUNK as u64) as usize);
        let lists = charge::array::<AtomicU64>(CHUNK).saturating_mul(full as usize).saturating_add(charge::array::<AtomicU64>(rest));
        let core = Self::new_core_for(job, charge::array::<Box<[AtomicU64]>>(count as usize).saturating_add(lists))?;
        let mut pieces = Vec::with_capacity(count as usize);
        let mut left = pages as usize;
        while left > 0 {
            let n = left.min(CHUNK);
            pieces.push((0..n).map(|_| AtomicU64::new(0)).collect());
            left -= n;
        }
        let object = Self::new(core, Backing::Frames(pieces), pages * PAGE_SIZE, CachePolicy::Normal);
        if committed > 0 {
            object.commit(0, committed)?;
        }
        Ok(object)
    }

    /// Commit every page `[offset, offset + len)` touches that is not yet,
    /// each a zeroed frame counted to the object's job: refused whole,
    /// `ResourceExhausted`, where its frames cannot all be promised.
    /// `InvalidArgument` for no bytes, a range past the object, or an
    /// aperture; memory with no page to commit answers at once.
    pub fn commit(&self, offset: u64, len: u64) -> Result<(), SyscallError> {
        if len == 0 || offset.checked_add(len).is_none_or(|end| end > self.size) {
            return Err(SyscallError::InvalidArgument);
        }
        let pieces = match &self.backing {
            Backing::Frames(pieces) => pieces,
            Backing::Sub { whole, offset: base, .. } => return whole.commit(base + offset, len),
            Backing::Run(_) | Backing::Boot { .. } => return Ok(()),
            Backing::Aperture { .. } => return Err(SyscallError::InvalidArgument),
        };
        let words = || {
            let (first, end) = (offset / PAGE_SIZE, (offset + len).div_ceil(PAGE_SIZE));
            (first..end).map(|page| &pieces[page as usize / CHUNK][page as usize % CHUNK])
        };
        let wanted = words().filter(|word| word.load(Ordering::Acquire) == 0).count() as u64;
        // Promised whole before any is taken, as a creation's are.
        if !pmm::promise(self.core.job(), wanted) {
            return Err(SyscallError::ResourceExhausted);
        }
        let mut drawn = 0;
        // A page another commit filled meanwhile was counted here and is
        // left; one it fills between the look and the store keeps its frame.
        for word in words().filter(|word| word.load(Ordering::Acquire) == 0) {
            let frame = pmm::alloc_promised().into_raw();
            drawn += 1;
            // Release: a fault that reads the frame finds it zeroed.
            if word.compare_exchange(0, frame, Ordering::Release, Ordering::Acquire).is_err() {
                // SAFETY: drawn just now and named by nothing.
                unsafe { pmm::free_drawn(self.core.job(), [frame]) };
            }
        }
        pmm::unpromise(self.core.job(), wanted - drawn);
        Ok(())
    }

    /// The bytes of it that are committed.
    pub fn committed(&self) -> u64 {
        match &self.backing {
            Backing::Frames(pieces) => {
                let pages = pieces.iter().flat_map(|piece| piece.iter()).filter(|word| word.load(Ordering::Relaxed) != 0).count();
                pages as u64 * PAGE_SIZE
            }
            Backing::Sub { whole, offset, len } => {
                (0..len.div_ceil(PAGE_SIZE)).filter(|page| whole.page(offset + page * PAGE_SIZE).is_some()).count() as u64 * PAGE_SIZE
            }
            Backing::Run(_) | Backing::Boot { .. } | Backing::Aperture { .. } => self.size,
        }
    }

    /// `size` bytes of zeroed memory in whole pages, over one run of whole
    /// 2 MiB pages: what a device can be lent, since a domain maps 2 MiB at a
    /// time and a device with no domain reaches memory by its physical address.
    /// Counted to `job`.
    pub fn create_run(job: &Arc<Job>, size: u64) -> Result<Arc<Self>, SyscallError> {
        let bytes = pages(size)? * PAGE_SIZE;
        let run = crate::mm::align_2m_checked(bytes).ok_or(SyscallError::InvalidArgument)?;
        let count = (run / PAGE_2M) as usize;
        let core = Self::new_core_for(job, 0)?;
        let run = Run::new(job, count).ok_or(SyscallError::ResourceExhausted)?;
        Ok(Self::new(core, Backing::Run(run), bytes, CachePolicy::Normal))
    }

    /// An aperture over `size` bytes of memory somebody else owns at `phys`,
    /// whole 4 KiB pages, mapped as `cache` says: never freed here, and taken
    /// out of every space that mapped it when its last handle goes.
    pub fn aperture(phys: u64, size: u64, cache: CachePolicy) -> Result<Arc<Self>, Exhausted> {
        assert!(
            (phys | size) & (PAGE_SIZE - 1) == 0 && size != 0,
            "shm: an aperture of {size:#x} bytes at {phys:#x} is not whole 4 KiB pages"
        );
        let mapped = Held::new((Vec::new(), Charge::none(&charge::current())));
        Ok(Self::new(Self::new_core(0)?, Backing::Aperture { phys, mapped }, size, cache))
    }

    /// A boot image of `len` bytes at `phys`, in whole 4 KiB pages: never
    /// freed and never written.
    pub fn boot(phys: u64, len: u64) -> Result<Arc<Self>, Exhausted> {
        assert!(phys.is_multiple_of(PAGE_SIZE) && len != 0, "shm: a boot image of {len:#x} bytes at {phys:#x}");
        Ok(Self::new(Self::new_core(0)?, Backing::Boot { phys, len }, len.next_multiple_of(PAGE_SIZE), CachePolicy::Normal))
    }

    /// `len` bytes of `object`'s from `offset`, a whole page in, as an
    /// object of their own over the same pages. Refused `InvalidArgument` for
    /// no bytes, a range past what `object` holds — its payload, where nobody
    /// writes it — or an aperture, whose last handle takes it back from every
    /// mapping.
    pub fn sub(object: &Arc<Self>, offset: u64, len: u64) -> Result<Arc<Self>, SyscallError> {
        let (whole, base) = match &object.backing {
            Backing::Sub { whole, offset, .. } => (whole, *offset),
            Backing::Aperture { .. } => return Err(SyscallError::InvalidArgument),
            Backing::Frames(_) | Backing::Run(_) | Backing::Boot { .. } => (object, 0),
        };
        let held = object.immutable().unwrap_or(object.size);
        if len == 0 || !offset.is_multiple_of(PAGE_SIZE) || offset.checked_add(len).is_none_or(|end| end > held) {
            return Err(SyscallError::InvalidArgument);
        }
        let size = pages(len)? * PAGE_SIZE;
        let backing = Backing::Sub { whole: Arc::clone(whole), offset: base + offset, len };
        Ok(Self::new(Self::new_core(0)?, backing, size, whole.cache))
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    /// The memory type every mapping of it carries: SDM Vol. 3A §11.12.4
    /// forbids one physical page under two.
    pub fn cache(&self) -> CachePolicy {
        self.cache
    }

    /// Whether its bytes are memory the kernel allocated to it, rather than a
    /// device's, firmware's or the loader's.
    pub fn is_ram(&self) -> bool {
        match &self.backing {
            Backing::Frames(_) | Backing::Run(_) => true,
            Backing::Sub { whole, .. } => whole.is_ram(),
            Backing::Boot { .. } | Backing::Aperture { .. } => false,
        }
    }

    /// The payload's length, for memory nobody writes: a boot image or part
    /// of one.
    pub fn immutable(&self) -> Option<u64> {
        match &self.backing {
            Backing::Boot { len, .. } => Some(*len),
            Backing::Sub { whole, len, .. } => whole.immutable().map(|_| *len),
            Backing::Frames(_) | Backing::Run(_) | Backing::Aperture { .. } => None,
        }
    }

    /// Whether a program may be read from it: ordinary memory, never a
    /// device's, whose read is a device access.
    pub fn is_ordinary(&self) -> bool {
        !matches!(self.backing, Backing::Aperture { .. })
    }

    /// The physical run and its length in whole 2 MiB pages, for ordinary
    /// memory the kernel allocated in one run — the only kind a device may be
    /// lent (`pcidev::dma_map`). `None` for anything else, a part of a run
    /// included: one set of pages is never lent under two objects.
    pub fn run(&self) -> Option<(u64, u64)> {
        match &self.backing {
            Backing::Run(run) => Some((run.direct_map().phys(), run.count() as u64 * PAGE_2M)),
            Backing::Frames(_) | Backing::Boot { .. } | Backing::Aperture { .. } | Backing::Sub { .. } => None,
        }
    }

    /// The physical address of the page `offset` bytes in, which lies inside
    /// it; `None` while that page is not committed.
    pub fn page(&self, offset: u64) -> Option<u64> {
        assert!(offset < self.size, "shm koid {}: page {offset:#x} of {:#x} bytes", self.core.koid().raw(), self.size);
        let page = offset & !(PAGE_SIZE - 1);
        match &self.backing {
            Backing::Frames(pieces) => {
                let frame = (page / PAGE_SIZE) as usize;
                // Acquire: the commit's zeroing is seen before the frame is.
                Some(pieces[frame / CHUNK][frame % CHUNK].load(Ordering::Acquire)).filter(|&phys| phys != 0)
            }
            Backing::Run(run) => Some(run.direct_map().phys() + page),
            Backing::Boot { phys, .. } | Backing::Aperture { phys, .. } => Some(phys + page),
            Backing::Sub { whole, offset, .. } => whole.page(offset + page),
        }
    }

    /// Copy `bytes` in at `offset`, through the direct map. For memory the
    /// kernel lays out before a process can see it, and the words it alone
    /// writes after; never an aperture, and only over committed pages.
    pub fn write(&self, offset: u64, bytes: &[u8]) {
        assert!(self.is_ram(), "shm koid {}: the kernel writes no aperture", self.core.koid().raw());
        self.each_piece(offset, bytes.len(), |phys, at, n| {
            // SAFETY: `each_piece` bounds `[at, at + n)` inside `bytes` and
            // `phys..+n` inside one of this object's own pages, which the
            // direct map covers; no reference forms over a page a process may
            // map, and the two cannot overlap.
            unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr().add(at), DirectMap::from_phys(phys).as_mut_ptr(), n) }
        });
    }

    /// Copy out `out.len()` bytes at `offset`, each read once.
    pub fn read(&self, offset: u64, out: &mut [u8]) {
        assert!(self.is_ordinary(), "shm koid {}: the kernel reads no aperture", self.core.koid().raw());
        let into = out.as_mut_ptr();
        self.each_piece(offset, out.len(), |phys, at, n| {
            // SAFETY: as `write`, mirrored; `out` is the caller's own.
            unsafe { core::ptr::copy_nonoverlapping(DirectMap::from_phys(phys).as_ptr(), into.add(at), n) }
        });
    }

    /// `f(phys, at, n)` for each piece of `[offset, offset + len)` inside one
    /// page, `at` its offset from `offset`.
    fn each_piece(&self, offset: u64, len: usize, mut f: impl FnMut(u64, usize, usize)) {
        assert!(
            offset.checked_add(len as u64).is_some_and(|end| end <= self.size),
            "shm koid {}: {len} bytes at {offset:#x} of {:#x}",
            self.core.koid().raw(),
            self.size
        );
        let mut done = 0;
        while done < len {
            let at = offset + done as u64;
            let n = ((PAGE_SIZE - at % PAGE_SIZE) as usize).min(len - done);
            let page = self.page(at).unwrap_or_else(|| panic!("shm koid {}: the kernel reached {at:#x}, not committed", self.core.koid().raw()));
            f(page + at % PAGE_SIZE, done, n);
            done += n;
        }
    }

    /// Record that `space` maps this object at `[at, at + len)`, if it is an
    /// aperture: what its last handle takes back. `Gone` once that has, and
    /// `ResourceExhausted` where the list cannot grow.
    pub fn mapped_at(&self, space: &Arc<Space>, at: UserAddr, len: u64) -> Result<(), SyscallError> {
        let Backing::Aperture { mapped, .. } = &self.backing else { return Ok(()) };
        mapped
            .with_mut(|(mapped, charge)| {
                mapped.retain(|(space, _, _)| space.strong_count() > 0);
                if mapped.len() == mapped.capacity() {
                    let capacity = (mapped.capacity() * 2).max(4);
                    charge.resize(charge::array::<Mapped>(capacity))?;
                    mapped.reserve_exact(capacity - mapped.len());
                }
                mapped.push((Arc::downgrade(space), at, len));
                Ok(())
            })
            .ok_or(SyscallError::Gone)?
    }
}

/// `size` bytes in whole pages, refused for none or one past every page count.
fn pages(size: u64) -> Result<u64, SyscallError> {
    match size.checked_next_multiple_of(PAGE_SIZE) {
        Some(0) | None => Err(SyscallError::InvalidArgument),
        Some(bytes) => Ok(bytes / PAGE_SIZE),
    }
}

/// The frames go back once no space maps the object and nothing holds it:
/// first every futex wait on a word in them ends, so none is left keyed on a
/// frame the allocator hands out again. Nothing is allocated: each piece is
/// sorted in place, its waits ended by one walk of the 64 buckets, and its
/// committed frames freed under one hold of the allocator's lock.
impl Drop for SharedMemObject {
    fn drop(&mut self) {
        match &mut self.backing {
            Backing::Frames(pieces) => {
                for mut piece in pieces.drain(..) {
                    piece.sort_unstable_by_key(|word| word.load(Ordering::Relaxed));
                    let committed = piece.partition_point(|word| word.load(Ordering::Relaxed) == 0);
                    let frames = &piece[committed..];
                    crate::sched::futex::revoke_sorted(frames, |word| word.load(Ordering::Relaxed));
                    // SAFETY: the last mapping and handle are gone, so no leaf
                    // maps a frame of it, and nothing else frees one.
                    unsafe { pmm::free_drawn(self.core.job(), frames.iter().map(|word| word.load(Ordering::Relaxed))) };
                }
            }
            Backing::Run(run) => {
                crate::sched::futex::revoke_run(run.direct_map().phys(), run.count() as u64 * PAGE_2M);
            }
            Backing::Boot { .. } | Backing::Aperture { .. } | Backing::Sub { .. } => {}
        }
    }
}

/// An aperture's last handle takes it out of every space that still maps it.
impl ZeroHandles for SharedMemObject {
    fn on_zero_handles(&self) {
        let Backing::Aperture { mapped, .. } = &self.backing else { return };
        let Some((mapped, _charge)) = mapped.take() else { return };
        for (space, at, len) in mapped {
            if let Some(space) = space.upgrade() {
                space.release_object(at, len, self);
            }
        }
    }
}
