//! A user address space: what it holds, as `kernel::vm`'s inventory records
//! it, and the tables that map it, under one lock.
//!
//! **The inventory is the only record of a space's ranges, and a leaf the only
//! record of a private frame**, which carries the architecture's owned bit. A
//! range is backed by private memory, committed or only reserved, or by a
//! memory object. Every page of either is populated when it is first touched —
//! by the thread that touches it ([`AddressSpace::fault`]) or by a copy the
//! kernel makes for it ([`AddressSpace::reach`]) — and tables are built when a
//! range is backed, so populating one takes a frame and nothing else.
//!
//! **Taking a range away is one hold of the lock**: its leaves stop mapping,
//! every CPU drops what it cached of them, and only then do its frames and the
//! tables nothing else needs go back. A thread faulting on the range meanwhile
//! waits on the lock, whose spin answers the shootdown, and finds it gone.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering::Relaxed};

use kernel::vm::{self, Access, Entry, Kind, Prot};
use toyos_abi::syscall::SyscallError;

use super::paging::{Leaf, Mapping, Root, Unlinked, UserTables};
use super::policy::{self, CachePolicy};
use super::{pmm, PAGE_2M, PAGE_SIZE};
use crate::object::shm::SharedMemObject;
use crate::sync::{Lock, LockGuard};
use crate::UserAddr;

type Inventory = vm::Inventory<Arc<SharedMemObject>, Arc<dyn CopySource>>;
type SpaceEntry = Entry<Arc<SharedMemObject>, Arc<dyn CopySource>>;
type SpaceKind = Kind<Arc<SharedMemObject>, Arc<dyn CopySource>>;

/// The bytes a copied range's pages start as. A read runs under the space
/// lock, from a fault or a user copy, so it may take no lock a user copy can
/// hold, and it cannot fail: what it reads was fixed when the range was made.
pub trait CopySource: Send + Sync {
    fn size(&self) -> u64;
    /// `out.len()` bytes from `offset`, all inside [`size`](Self::size).
    fn read(&self, offset: u64, out: &mut [u8]);
}

impl CopySource for SharedMemObject {
    fn size(&self) -> u64 {
        SharedMemObject::size(self)
    }

    fn read(&self, offset: u64, out: &mut [u8]) {
        SharedMemObject::read(self, offset, out)
    }
}

/// Nothing is placed below this, so a null pointer and the pages above it
/// fault.
pub const FLOOR: u64 = 0x10_0000;

/// One past the last page placed: the user half less its last 2 MiB, so no
/// instruction ends at the first address that is not canonical
/// (`toyos_userbound`'s `PLACED_TOP`).
pub const TOP: u64 = toyos_userbound::USER_TOP - PAGE_2M;

/// Where a new range goes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum At {
    /// Wherever it fits, on a multiple of the given power of two.
    Anywhere(u64),
    /// Exactly here, in place of whatever the range held.
    Fixed(UserAddr),
}

impl At {
    pub const ANYWHERE: Self = Self::Anywhere(PAGE_SIZE);
}

/// What a space holds, read without its lock: by `SYS_SYSINFO`, by a
/// process's statistics, and by its end's record.
#[derive(Default)]
pub struct Usage {
    /// Private frames populated now.
    resident: AtomicU64,
    peak: AtomicU64,
    /// Ranges made and taken away.
    made: AtomicU64,
    taken: AtomicU64,
    /// Pages a fault populated, and the time faults took.
    faults: AtomicU64,
    fault_ns: AtomicU64,
    /// Changes to the space's leaves that reached another CPU by interrupt,
    /// for the test kernel's `SYS_DEBUG`.
    #[cfg(feature = "test-actuators")]
    reached: AtomicU64,
}

impl Usage {
    #[cfg(feature = "test-actuators")]
    pub fn reached(&self) -> u64 {
        self.reached.load(Relaxed)
    }

    pub fn resident_bytes(&self) -> u64 {
        self.resident.load(Relaxed) * PAGE_SIZE
    }

    pub fn peak_bytes(&self) -> u64 {
        self.peak.load(Relaxed) * PAGE_SIZE
    }

    pub fn ranges_made(&self) -> u64 {
        self.made.load(Relaxed)
    }

    pub fn ranges_taken(&self) -> u64 {
        self.taken.load(Relaxed)
    }

    pub fn faults(&self) -> u64 {
        self.faults.load(Relaxed)
    }

    pub fn fault_ns(&self) -> u64 {
        self.fault_ns.load(Relaxed)
    }

    /// A fault populated a page, in `ns`.
    pub fn faulted(&self, ns: u64) {
        self.faults.fetch_add(1, Relaxed);
        self.fault_ns.fetch_add(ns, Relaxed);
    }

    fn populated(&self, frames: u64) {
        let now = self.resident.fetch_add(frames, Relaxed) + frames;
        self.peak.fetch_max(now, Relaxed);
    }

    fn freed(&self, frames: u64) {
        let was = self.resident.fetch_sub(frames, Relaxed);
        assert!(was >= frames, "space: {frames} frames freed of {was} resident");
    }
}

/// A space and the lock its every change takes.
pub struct Space {
    inner: Lock<AddressSpace>,
    usage: Arc<Usage>,
}

/// How every thread of a process, and the scheduler, names its space.
pub type PageTables = Arc<Space>;

impl Space {
    /// A new user space holding nothing, or `None` when every tag the
    /// architecture has for one is held by a live space.
    pub fn new_user() -> Option<PageTables> {
        let usage = Arc::new(Usage::default());
        let space = AddressSpace {
            inventory: Inventory::new(FLOOR, TOP),
            tables: UserTables::new()?,
            usage: Arc::clone(&usage),
            kernel: false,
        };
        Some(Arc::new(Self { inner: Lock::new(space), usage }))
    }

    #[track_caller]
    pub fn lock(&self) -> LockGuard<'_, AddressSpace> {
        self.inner.lock()
    }

    #[track_caller]
    pub fn try_lock(&self) -> Option<LockGuard<'_, AddressSpace>> {
        self.inner.try_lock()
    }

    pub fn usage(&self) -> &Usage {
        &self.usage
    }

    /// Take `[at, at + len)` away if it still maps `object` and nothing else.
    pub fn release_object(&self, at: UserAddr, len: u64, object: &SharedMemObject) {
        self.lock().release_object(at, len, object);
    }
}

/// The kernel's own space: what a kernel thread and an idle CPU run under,
/// holding nothing in its user half. Made once, after the kernel's tables.
static KERNEL: core::sync::atomic::AtomicPtr<PageTables> = core::sync::atomic::AtomicPtr::new(core::ptr::null_mut());

/// Make the kernel's space; `mm::init` calls it once its tables are built.
pub(super) fn init_kernel() {
    let usage = Arc::new(Usage::default());
    let space = AddressSpace { inventory: Inventory::new(FLOOR, TOP), tables: UserTables::kernel(), usage: Arc::clone(&usage), kernel: true };
    // Leaked: it outlives every task, and its tables are the kernel's.
    let space: &'static PageTables = alloc::boxed::Box::leak(alloc::boxed::Box::new(Arc::new(Space { inner: Lock::new(space), usage })));
    KERNEL.store(core::ptr::from_ref(space).cast_mut(), core::sync::atomic::Ordering::Release);
}

pub fn kernel() -> PageTables {
    let space = KERNEL.load(core::sync::atomic::Ordering::Acquire);
    assert!(!space.is_null(), "space: the kernel's before `mm::init`");
    // SAFETY: `init_kernel` stored a leaked box's pointer, which is never
    // cleared or freed.
    Arc::clone(unsafe { &*space })
}

pub struct AddressSpace {
    inventory: Inventory,
    tables: UserTables,
    usage: Arc<Usage>,
    kernel: bool,
}

/// What a leaf of a range with protection `prot` says, `None` for one that
/// maps nothing.
fn leaf_prot(prot: Prot) -> Option<policy::Prot> {
    match prot {
        Prot::None => None,
        Prot::Read => Some(policy::Prot::Read),
        Prot::ReadWrite => Some(policy::Prot::ReadWrite),
        Prot::ReadExec => Some(policy::Prot::ReadExec),
    }
}

/// Whether `[start, start + len)` is a non-empty run of whole pages.
fn pages(start: u64, len: u64) -> Option<u64> {
    let end = start.checked_add(len)?;
    (len != 0 && start.is_multiple_of(PAGE_SIZE) && len.is_multiple_of(PAGE_SIZE)).then_some(end)
}

impl AddressSpace {
    pub fn root(&self) -> Root {
        self.tables.root()
    }

    /// Where a range of `len` bytes goes, changing nothing: a `Fixed` one is
    /// put in place of what is there by [`put`](Self::put). `len` is whole
    /// pages.
    fn place(&self, len: u64, at: At) -> Result<u64, SyscallError> {
        match at {
            At::Fixed(start) => {
                let end = pages(start.raw(), len).ok_or(SyscallError::InvalidArgument)?;
                if !self.inventory.holds(start.raw(), end) {
                    return Err(SyscallError::InvalidArgument);
                }
                Ok(start.raw())
            }
            At::Anywhere(align) => {
                // A length no placement in the space could ever hold is the
                // caller's mistake; one that finds no room now is the machine's.
                let room = self.inventory.top() - self.inventory.floor();
                if !align.is_power_of_two() || len == 0 || !len.is_multiple_of(PAGE_SIZE) || len > room {
                    return Err(SyscallError::InvalidArgument);
                }
                self.inventory.place(len, align.max(PAGE_SIZE)).ok_or(SyscallError::ResourceExhausted)
            }
        }
    }

    /// Hold `entry` from `start`, in place of whatever those pages held: the
    /// new range is in the inventory before the old leaves are cleared, so the
    /// clear keeps the tables it needs. It cannot be refused.
    fn put(&mut self, start: u64, entry: SpaceEntry) {
        let end = entry.end;
        if self.inventory.is_free(start, end) {
            self.inventory.insert(start, entry);
        } else {
            let removed = self.inventory.remove(start, end);
            let committed: u64 = removed
                .iter()
                .filter(|(_, old)| old.committed())
                .map(|(s, old)| (old.end - s) / PAGE_SIZE)
                .sum();
            self.inventory.insert(start, entry);
            let freed = self.clear(start, end);
            pmm::unpromise(committed - freed);
            self.usage.taken.fetch_add(1, Relaxed);
            // The objects go here, after every CPU has stopped reaching them.
            drop(removed);
        }
        self.usage.made.fetch_add(1, Relaxed);
    }

    /// `ResourceExhausted` unless the space may still hold `added` ranges
    /// more and those a change over `[start, end)` adds by cutting a range at
    /// either end. The bound keeps what a space's ranges take of the kernel
    /// heap finite.
    pub fn room(&self, start: u64, end: u64, added: usize) -> Result<(), SyscallError> {
        let cut = |at: u64| self.inventory.find(at).is_some_and(|(s, _)| s != at);
        let cuts = usize::from(cut(start)) + usize::from(cut(end));
        if self.inventory.len() + added + cuts > toyos_abi::syscall::MAX_RANGES {
            return Err(SyscallError::ResourceExhausted);
        }
        Ok(())
    }

    /// [`room`](Self::room) for one range more placed `at`.
    fn room_at(&self, len: u64, at: At) -> Result<(), SyscallError> {
        match at {
            At::Fixed(start) => self.room(start.raw(), start.raw().saturating_add(len), 1),
            At::Anywhere(_) => self.room(0, 0, 1),
        }
    }

    /// `len` bytes of private memory, committed unless `prot` is `None`, which
    /// reserves the range and nothing more. A commitment is promised its
    /// frames here, so it is refused here, `ResourceExhausted`, and never at a
    /// later touch. All or nothing: a refused `Fixed` range leaves what the
    /// pages held; one that is not refused needs its frames promised beside
    /// those of what it replaces.
    pub fn reserve(&mut self, len: u64, at: At, prot: Prot) -> Result<UserAddr, SyscallError> {
        self.private(len, at, prot, Kind::Private { committed: prot != Prot::None })
    }

    /// `len` bytes of private memory, committed, whose pages start as a copy
    /// of `source` from `offset` as far as their first `copied` bytes go and
    /// zeros past them, each taken at its first touch: a program's segment
    /// read lazily out of its file. Promised and all or nothing, as
    /// [`reserve`](Self::reserve).
    pub fn copy(&mut self, len: u64, at: At, prot: Prot, source: Arc<dyn CopySource>, offset: u64, copied: u64) -> Result<UserAddr, SyscallError> {
        self.private(len, at, prot, Kind::Copy { source, offset, len: copied })
    }

    fn private(&mut self, len: u64, at: At, prot: Prot, kind: SpaceKind) -> Result<UserAddr, SyscallError> {
        // A length no placement could hold is the caller's mistake, whatever
        // memory is left to promise.
        if len > self.inventory.top() - self.inventory.floor() {
            return Err(SyscallError::InvalidArgument);
        }
        self.room_at(len, at)?;
        let start = self.place(len, at)?;
        let entry = Entry { end: start + len, prot, kind };
        let promised = if entry.committed() { len / PAGE_SIZE } else { 0 };
        if !pmm::promise(promised) {
            return Err(SyscallError::ResourceExhausted);
        }
        if entry.backed() {
            self.build(start, start + len).inspect_err(|_| pmm::unpromise(promised))?;
        }
        self.put(start, entry);
        Ok(UserAddr::new(start))
    }

    /// Build the tables `[start, end)` needs, or none of those it needs alone:
    /// `ResourceExhausted` where taking a frame would cut into what the
    /// kernel keeps.
    fn build(&mut self, start: u64, end: u64) -> Result<(), SyscallError> {
        if self.tables.build(start, end).is_some() {
            return Ok(());
        }
        let inventory = &self.inventory;
        let unlinked = self.tables.unlink(start, end, |s, e| inventory.backed(s, e));
        if !unlinked.is_empty() {
            self.tables.shoot_down(start, end, &unlinked);
        }
        unlinked.free();
        Err(SyscallError::ResourceExhausted)
    }

    /// Committed private pages of `[start, end)`.
    fn committed_pages(&self, start: u64, end: u64) -> u64 {
        self.inventory
            .meeting(start, end)
            .filter(|(_, entry)| entry.committed())
            .map(|(s, entry)| (entry.end.min(end) - s.max(start)) / PAGE_SIZE)
            .sum()
    }

    /// Pages of `[start, end)` whose range's kind `which` answers `true` for.
    fn pages_where(&self, start: u64, end: u64, which: impl Fn(&SpaceKind) -> bool) -> u64 {
        self.inventory
            .meeting(start, end)
            .filter(|(_, entry)| which(&entry.kind))
            .map(|(s, entry)| (entry.end.min(end) - s.max(start)) / PAGE_SIZE)
            .sum()
    }

    /// `len` bytes of `object` from `offset`, as `prot` allows; the caller
    /// bounds `prot` by what its handle grants. All or nothing, as
    /// [`reserve`](Self::reserve).
    pub fn map(&mut self, object: Arc<SharedMemObject>, offset: u64, len: u64, prot: Prot, at: At) -> Result<UserAddr, SyscallError> {
        let within = pages(offset, len).is_some_and(|end| end <= object.size());
        if !within {
            return Err(SyscallError::InvalidArgument);
        }
        self.room_at(len, at)?;
        let start = self.place(len, at)?;
        self.build(start, start + len)?;
        self.put(start, Entry { end: start + len, prot, kind: Kind::Object { object, offset, max: prot } });
        Ok(UserAddr::new(start))
    }

    /// Take every page of `[start, start + len)` away, whatever holds it: the
    /// POSIX `munmap` of any run of pages. Nothing held there is not a refusal.
    pub fn release(&mut self, start: UserAddr, len: u64) {
        let Some(end) = pages(start.raw(), len) else { return };
        let (start, end) = (start.raw().max(FLOOR), end.min(TOP));
        if start >= end {
            return;
        }
        let removed = self.inventory.remove(start, end);
        if removed.is_empty() {
            return;
        }
        let committed: u64 = removed
            .iter()
            .filter(|(_, entry)| entry.committed())
            .map(|(s, entry)| (entry.end - s) / PAGE_SIZE)
            .sum();
        let freed = self.clear(start, end);
        pmm::unpromise(committed - freed);
        self.usage.taken.fetch_add(1, Relaxed);
        // The objects go here, after every CPU has stopped reaching them.
        drop(removed);
    }

    /// Give every page of `[start, start + len)` protection `prot`: a reserved
    /// private page is committed unless `prot` is `None`, promised its frame
    /// here, and a committed one keeps its frame and commitment whatever
    /// `prot` is. An object's page takes nothing past what it was mapped
    /// with. All or nothing: every page is held (`BadAddress`), allowed `prot`
    /// (`PermissionDenied`) and promised (`ResourceExhausted`) first. Every
    /// CPU stops using a present leaf's old protection before this returns, a
    /// widening included.
    pub fn protect(&mut self, start: UserAddr, len: u64, prot: Prot) -> Result<(), SyscallError> {
        let (start, end) = self.held(start, len)?;
        self.room(start, end, 0)?;
        let allowed = self.inventory.meeting(start, end).all(|(_, entry)| match entry.kind {
            Kind::Object { max, .. } => prot.within(max),
            Kind::Private { .. } | Kind::Copy { .. } => true,
        });
        if !allowed {
            return Err(SyscallError::PermissionDenied);
        }
        let commits = match prot {
            Prot::None => 0,
            _ => self.pages_where(start, end, |kind| matches!(kind, Kind::Private { committed: false })),
        };
        if !pmm::promise(commits) {
            return Err(SyscallError::ResourceExhausted);
        }
        if commits > 0 {
            self.build(start, end).inspect_err(|_| pmm::unpromise(commits))?;
        }
        self.inventory.update(start, end, |_, entry| {
            entry.prot = prot;
            if let Kind::Private { committed } = &mut entry.kind {
                *committed |= prot != Prot::None;
            }
        });
        let leaf = leaf_prot(prot);
        let mut present = false;
        self.tables.edit(start, end, |_, old| match (old, leaf) {
            (Leaf::Mapped(m), Some(p)) if m.prot == p => None,
            (Leaf::Mapped(m), Some(p)) => {
                present = true;
                Some(Leaf::Mapped(Mapping { prot: p, ..m }))
            }
            (Leaf::Mapped(m), None) => {
                present = true;
                Some(if m.owned { Leaf::Held(m.phys) } else { Leaf::Empty })
            }
            (Leaf::Held(phys), Some(p)) => Some(Leaf::Mapped(Mapping { phys, prot: p, owned: true, cache: CachePolicy::Normal })),
            (Leaf::Held(_), None) | (Leaf::Empty, _) => None,
        });
        if present {
            self.shoot_down(start, end, &Unlinked::none());
        }
        Ok(())
    }

    /// Free every frame behind `[start, start + len)` and keep its ranges as
    /// they are: a private page reads zeros at its next touch, a copied one
    /// its source again, an object's page is mapped again from the object.
    ///
    /// A private page's frame is freed and the page promised a frame again;
    /// one a user copy pins is zeroed in place instead and kept, since freed
    /// it would come back only at its last unpin and the promise would be
    /// backed by nothing meanwhile (`Frames::discard`). No pin is taken
    /// meanwhile: a copy pins under this space's lock, which this holds.
    pub fn discard(&mut self, start: UserAddr, len: u64) -> Result<(), SyscallError> {
        let (start, end) = self.held(start, len)?;
        let mut present = false;
        self.tables.edit(start, end, |_, leaf| match leaf {
            Leaf::Mapped(Mapping { phys, owned: true, .. }) => {
                present = true;
                Some(Leaf::Held(phys))
            }
            Leaf::Mapped(_) => {
                present = true;
                Some(Leaf::Empty)
            }
            Leaf::Held(_) | Leaf::Empty => None,
        });
        if present {
            self.tables.shoot_down(start, end, &Unlinked::none());
        }
        let mut freed = 0;
        let inventory = &self.inventory;
        self.tables.discard_owned(start, end, |frames, vas, kept| {
            let mut sorted = [0u64; 512];
            let sorted = &mut sorted[..frames.len()];
            sorted.copy_from_slice(frames);
            sorted.sort_unstable();
            crate::sched::futex::revoke_frames(sorted);
            for ((&phys, &va), kept) in frames.iter().zip(vas).zip(kept.iter_mut()) {
                // SAFETY: a frame an owned leaf held, which no leaf on any CPU
                // maps now and no waiter is keyed on; one kept is the page's.
                *kept = match unsafe { pmm::discard(phys) } {
                    pmm::Discarded::Freed => {
                        freed += 1;
                        false
                    }
                    pmm::Discarded::Kept => {
                        // SAFETY: the page's own frame, which a copy may be
                        // reading or writing as the page's contents; zeroing
                        // it is what the discard makes those contents.
                        unsafe { core::ptr::write_bytes(super::DirectMap::from_phys(phys).as_mut_ptr::<u8>(), 0, PAGE_SIZE as usize) };
                        // A copied page's contents start as its source's again.
                        if let Some((s, Entry { kind: Kind::Copy { source, offset, len }, .. })) = inventory.find(va) {
                            copy_in(&**source, offset + (va - s), len.saturating_sub(va - s), phys);
                        }
                        true
                    }
                };
            }
        });
        self.usage.freed(freed);
        Ok(())
    }

    /// Free every frame and the commitment of `[start, start + len)`, which
    /// is all private memory: it is reserved again.
    pub fn decommit(&mut self, start: UserAddr, len: u64) -> Result<(), SyscallError> {
        let (start, end) = self.held(start, len)?;
        self.room(start, end, 0)?;
        if !self.inventory.meeting(start, end).all(|(_, entry)| matches!(entry.kind, Kind::Private { .. } | Kind::Copy { .. })) {
            return Err(SyscallError::InvalidArgument);
        }
        let committed = self.committed_pages(start, end);
        self.inventory.update(start, end, |_, entry| {
            entry.prot = Prot::None;
            entry.kind = Kind::Private { committed: false };
        });
        let freed = self.clear(start, end);
        pmm::unpromise(committed - freed);
        Ok(())
    }

    /// `[start, start + len)` as pages every one of which a range holds.
    fn held(&self, start: UserAddr, len: u64) -> Result<(u64, u64), SyscallError> {
        let end = pages(start.raw(), len).ok_or(SyscallError::InvalidArgument)?;
        if !self.inventory.holds(start.raw(), end) || !self.inventory.covered(start.raw(), end) {
            return Err(SyscallError::BadAddress);
        }
        Ok((start.raw(), end))
    }

    /// Every CPU has dropped what it cached of `[start, end)` and of the
    /// tables `unlinked` took out.
    fn shoot_down(&self, start: u64, end: u64, unlinked: &Unlinked) {
        let _reached = self.tables.shoot_down(start, end, unlinked);
        #[cfg(feature = "test-actuators")]
        if _reached {
            self.usage.reached.fetch_add(1, Relaxed);
        }
    }

    /// [`release`](Self::release) of every page of `[start, start + len)`
    /// that still maps `object` where a mapping of it from its start at
    /// `start` would: a page the process unmapped or replaced since is left as
    /// it is, and the rest goes.
    pub fn release_object(&mut self, start: UserAddr, len: u64, object: &SharedMemObject) {
        let Some(end) = pages(start.raw(), len) else { return };
        let start = start.raw();
        let mut at = start;
        while at < end {
            let next = self.inventory.meeting(at, end).find_map(|(s, entry)| match &entry.kind {
                Kind::Object { object: held, offset, .. }
                    if core::ptr::eq(Arc::as_ptr(held), object) && *offset + at.saturating_sub(s) == at.max(s) - start =>
                {
                    Some((at.max(s), entry.end.min(end)))
                }
                _ => None,
            });
            let Some((from, to)) = next else { return };
            self.release(UserAddr::new(from), to - from);
            at = to;
        }
    }

    /// Stop every leaf of `[start, end)` mapping, drop what every CPU cached
    /// of them, end every futex wait on a frame they owned, then free those
    /// frames and every table over them no range the inventory still backs
    /// needs. Answers the frames freed.
    ///
    /// Nothing is allocated: the frames go one table of leaves at a time, at
    /// most 512, each batch sorted in place and its futex waits ended by one
    /// walk of the 64 buckets, so a clear costs one walk per table of leaves
    /// that held an owned frame and one shootdown, two where tables go too.
    fn clear(&mut self, start: u64, end: u64) -> u64 {
        let mut present = false;
        self.tables.edit(start, end, |_, leaf| match leaf {
            Leaf::Mapped(Mapping { phys, owned: true, .. }) => {
                present = true;
                Some(Leaf::Held(phys))
            }
            Leaf::Mapped(_) => {
                present = true;
                Some(Leaf::Empty)
            }
            Leaf::Held(_) | Leaf::Empty => None,
        });
        if present {
            self.shoot_down(start, end, &Unlinked::none());
        }
        let mut freed = 0;
        self.tables.take_owned(start, end, |frames| {
            frames.sort_unstable();
            crate::sched::futex::revoke_frames(frames);
            // SAFETY: frames owned leaves gave up, which no leaf maps on any
            // CPU now and no waiter is keyed on.
            unsafe { pmm::free_raw(frames) };
            freed += frames.len() as u64;
        });
        let inventory = &self.inventory;
        let unlinked = self.tables.unlink(start, end, |s, e| inventory.backed(s, e));
        if !unlinked.is_empty() {
            self.shoot_down(start, end, &unlinked);
        }
        unlinked.free();
        self.usage.freed(freed);
        freed
    }

    /// Serve a fault at `va` for `access`: populate its page if its range
    /// allows the access and the page is empty. `false` ends the thread.
    pub fn fault(&mut self, va: u64, access: Access) -> bool {
        self.reach(va, access).is_some()
    }

    /// Where the byte at `va` is, populating its page where its range allows
    /// `access`: what a copy the kernel makes for a thread of this space
    /// reaches, which is what a store or load of the thread's own would.
    pub fn reach(&mut self, va: u64, access: Access) -> Option<u64> {
        let (start, entry) = self.inventory.find(va)?;
        if !entry.prot.allows(access) {
            return None;
        }
        let page = va & !(PAGE_SIZE - 1);
        let phys = match self.tables.get(page) {
            Leaf::Mapped(m) => m.phys,
            // A frame the page keeps while no leaf maps it: a discard that
            // zeroed a pinned frame in place.
            Leaf::Held(phys) => {
                let prot = leaf_prot(entry.prot)?;
                self.tables.set(page, Leaf::Mapped(Mapping { phys, prot, owned: true, cache: CachePolicy::Normal }));
                phys
            }
            Leaf::Empty => {
                let leaf = page_leaf(start, entry, page)?;
                let Leaf::Mapped(m) = leaf else { return None };
                self.tables.set(page, leaf);
                if m.owned {
                    self.usage.populated(1);
                }
                m.phys
            }
        };
        Some(phys + va % PAGE_SIZE)
    }

    /// Whether every page of `[start, end)` is in a range that allows
    /// `access`: what a copy the kernel makes for a thread asks first.
    pub fn allows(&self, start: u64, end: u64, access: Access) -> bool {
        self.inventory.covered(start & !(PAGE_SIZE - 1), end)
            && self.inventory.meeting(start, end).all(|(_, entry)| entry.prot.allows(access) && entry.backed())
    }

    /// The frame and protection a present leaf maps `va` with, populating
    /// nothing: for a check made where no fault may be taken.
    pub fn get(&self, va: u64) -> Option<(u64, policy::Prot)> {
        match self.tables.get(va & !(PAGE_SIZE - 1)) {
            Leaf::Mapped(m) => Some((m.phys + va % PAGE_SIZE, m.prot)),
            Leaf::Held(_) | Leaf::Empty => None,
        }
    }

    /// Write `bytes` at `va`, into private memory this space committed:
    /// what the kernel lays out in a space before any thread runs in it, or
    /// in a range no thread has been told of.
    pub fn fill(&mut self, va: u64, bytes: &[u8]) {
        let mut done = 0;
        while done < bytes.len() {
            let at = va + done as u64;
            let n = ((PAGE_SIZE - at % PAGE_SIZE) as usize).min(bytes.len() - done);
            let phys = self.populate_private(at).unwrap_or_else(|| panic!("space: {at:#x} is no committed private memory"));
            // SAFETY: `populate_private` answered a frame this space owns and
            // maps at `at`, and `n` bytes from there stay inside its page; the
            // kernel's own buffer cannot overlap it.
            unsafe {
                core::ptr::copy_nonoverlapping(bytes.as_ptr().add(done), super::DirectMap::from_phys(phys).as_mut_ptr(), n);
            }
            if self.inventory.find(at).is_some_and(|(_, entry)| entry.prot == Prot::ReadExec) {
                super::paging::wrote_code(phys & !(PAGE_SIZE - 1));
            }
            done += n;
        }
    }

    /// `va`'s frame, populated if empty, where `va` is committed private memory.
    fn populate_private(&mut self, va: u64) -> Option<u64> {
        let (_, entry) = self.inventory.find(va)?;
        if !matches!(entry.kind, Kind::Private { committed: true }) {
            return None;
        }
        let page = va & !(PAGE_SIZE - 1);
        let phys = match self.tables.get(page) {
            Leaf::Mapped(m) => m.phys,
            Leaf::Held(phys) => phys,
            Leaf::Empty => {
                let phys = pmm::alloc_promised().into_raw();
                let leaf = match leaf_prot(entry.prot) {
                    Some(prot) => Leaf::Mapped(Mapping { phys, prot, owned: true, cache: CachePolicy::Normal }),
                    None => Leaf::Held(phys),
                };
                self.tables.set(page, leaf);
                self.usage.populated(1);
                phys
            }
        };
        Some(phys + va % PAGE_SIZE)
    }
}

/// The leaf a fault puts at `page`, inside the range `entry` starting at
/// `start`: a fresh frame of private memory, or the object's own page.
fn page_leaf(start: u64, entry: &SpaceEntry, page: u64) -> Option<Leaf> {
    let prot = leaf_prot(entry.prot)?;
    Some(match &entry.kind {
        Kind::Private { committed: false } => return None,
        Kind::Private { committed: true } => {
            Leaf::Mapped(Mapping { phys: pmm::alloc_promised().into_raw(), prot, owned: true, cache: CachePolicy::Normal })
        }
        Kind::Object { object, offset, .. } => {
            Leaf::Mapped(Mapping { phys: object.page(offset + (page - start)), prot, owned: false, cache: object.cache() })
        }
        Kind::Copy { source, offset, len } => {
            let phys = pmm::alloc_promised().into_raw();
            copy_in(&**source, offset + (page - start), len.saturating_sub(page - start), phys);
            Leaf::Mapped(Mapping { phys, prot, owned: true, cache: CachePolicy::Normal })
        }
    })
}

/// Write `source`'s bytes from `offset` into the zeroed frame at `phys`, as
/// many of the page's as `len` and the source both hold.
fn copy_in(source: &dyn CopySource, offset: u64, len: u64, phys: u64) {
    let n = len.min(PAGE_SIZE).min(source.size().saturating_sub(offset)) as usize;
    if n == 0 {
        return;
    }
    // SAFETY: a frame just allocated to this page, which no leaf maps yet, so
    // nothing else reaches it; `n` bytes stay inside it.
    let frame = unsafe { core::slice::from_raw_parts_mut(super::DirectMap::from_phys(phys).as_mut_ptr::<u8>(), n) };
    source.read(offset, frame);
}

/// Every frame the space owns goes back; its tables go with `UserTables`.
///
/// Not through [`AddressSpace::clear`]: no thread runs in a space that drops,
/// and a private frame is mapped in no other, so no CPU caches its leaves, no
/// futex waiter is keyed on its frames and no copy pins them.
impl Drop for AddressSpace {
    fn drop(&mut self) {
        assert!(!self.kernel, "space: the kernel's dropped");
        let committed = self.committed_pages(FLOOR, TOP);
        self.tables.edit(FLOOR, TOP, |_, leaf| matches!(leaf, Leaf::Mapped(Mapping { owned: false, .. })).then_some(Leaf::Empty));
        let mut freed = 0;
        self.tables.take_owned(FLOOR, TOP, |frames| {
            // SAFETY: the space's own frames, which nothing maps once it drops.
            unsafe { pmm::free_raw(frames) };
            freed += frames.len() as u64;
        });
        self.usage.freed(freed);
        pmm::unpromise(committed - freed);
    }
}
