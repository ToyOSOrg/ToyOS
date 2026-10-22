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

type Inventory = vm::Inventory<Arc<SharedMemObject>>;

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
}

impl Usage {
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

    /// Where a range of `len` bytes goes; a `Fixed` one takes away whatever
    /// was there first. `len` is whole pages.
    fn locate(&mut self, len: u64, at: At) -> Result<u64, SyscallError> {
        match at {
            At::Fixed(start) => {
                let end = pages(start.raw(), len).ok_or(SyscallError::InvalidArgument)?;
                if !self.inventory.holds(start.raw(), end) {
                    return Err(SyscallError::InvalidArgument);
                }
                self.release(start, len);
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
    /// reserves the range and nothing more.
    pub fn reserve(&mut self, len: u64, at: At, prot: Prot) -> Result<UserAddr, SyscallError> {
        self.room_at(len, at)?;
        let start = self.locate(len, at)?;
        let committed = prot != Prot::None;
        self.inventory.insert(start, Entry { end: start + len, prot, kind: Kind::Private { committed } });
        if committed {
            self.tables.build(start, start + len);
        }
        self.usage.made.fetch_add(1, Relaxed);
        Ok(UserAddr::new(start))
    }

    /// `len` bytes of `object` from `offset`, as `prot` allows; the caller
    /// bounds `prot` by what its handle grants.
    pub fn map(&mut self, object: Arc<SharedMemObject>, offset: u64, len: u64, prot: Prot, at: At) -> Result<UserAddr, SyscallError> {
        let within = pages(offset, len).is_some_and(|end| end <= object.size());
        if !within {
            return Err(SyscallError::InvalidArgument);
        }
        self.room_at(len, at)?;
        let start = self.locate(len, at)?;
        self.inventory.insert(start, Entry { end: start + len, prot, kind: Kind::Object { object, offset, max: prot } });
        self.tables.build(start, start + len);
        self.usage.made.fetch_add(1, Relaxed);
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
        self.clear(start, end);
        self.usage.taken.fetch_add(1, Relaxed);
        // The objects go here, after every CPU has stopped reaching them.
        drop(removed);
    }

    /// Give every page of `[start, start + len)` protection `prot`: a reserved
    /// private page is committed unless `prot` is `None`, and a committed one
    /// keeps its frame and commitment whatever `prot` is. An object's page
    /// takes nothing past what it was mapped with. All or nothing: every page
    /// is held (`BadAddress`) and allowed `prot` (`PermissionDenied`) first.
    /// Every CPU stops using a present leaf's old protection before this
    /// returns, a widening included.
    pub fn protect(&mut self, start: UserAddr, len: u64, prot: Prot) -> Result<(), SyscallError> {
        let (start, end) = self.held(start, len)?;
        self.room(start, end, 0)?;
        let allowed = self.inventory.meeting(start, end).all(|(_, entry)| match entry.kind {
            Kind::Object { max, .. } => prot.within(max),
            Kind::Private { .. } => true,
        });
        if !allowed {
            return Err(SyscallError::PermissionDenied);
        }
        let mut commits = false;
        self.inventory.update(start, end, |_, entry| {
            entry.prot = prot;
            if let Kind::Private { committed } = &mut entry.kind {
                if !*committed && prot != Prot::None {
                    *committed = true;
                    commits = true;
                }
            }
        });
        if commits {
            self.tables.build(start, end);
        }
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
            self.tables.shoot_down(start, end, &Unlinked::none());
        }
        Ok(())
    }

    /// Free every frame behind `[start, start + len)` and keep its ranges as
    /// they are: a private page reads zeros at its next touch, an object's
    /// page is mapped again from the object.
    pub fn discard(&mut self, start: UserAddr, len: u64) -> Result<(), SyscallError> {
        let (start, end) = self.held(start, len)?;
        self.clear(start, end);
        Ok(())
    }

    /// Free every frame and the commitment of `[start, start + len)`, which
    /// is all private memory: it is reserved again.
    pub fn decommit(&mut self, start: UserAddr, len: u64) -> Result<(), SyscallError> {
        let (start, end) = self.held(start, len)?;
        self.room(start, end, 0)?;
        if !self.inventory.meeting(start, end).all(|(_, entry)| matches!(entry.kind, Kind::Private { .. })) {
            return Err(SyscallError::InvalidArgument);
        }
        self.inventory.update(start, end, |_, entry| {
            entry.prot = Prot::None;
            entry.kind = Kind::Private { committed: false };
        });
        self.clear(start, end);
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
            self.tables.shoot_down(start, end, &Unlinked::none());
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
            self.tables.shoot_down(start, end, &unlinked);
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
            Leaf::Held(_) => return None,
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
                let frame = super::pmm::alloc_frame().unwrap_or_else(|| panic!("space: no frame for {va:#x}"));
                let phys = frame.into_raw();
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
fn page_leaf(start: u64, entry: &Entry<Arc<SharedMemObject>>, page: u64) -> Option<Leaf> {
    let prot = leaf_prot(entry.prot)?;
    Some(match &entry.kind {
        Kind::Private { committed: false } => return None,
        Kind::Private { committed: true } => {
            let Some(frame) = super::pmm::alloc_frame() else {
                crate::log!("space: no frame to populate {page:#x}");
                return None;
            };
            Leaf::Mapped(Mapping { phys: frame.into_raw(), prot, owned: true, cache: CachePolicy::Normal })
        }
        Kind::Object { object, offset, .. } => {
            Leaf::Mapped(Mapping { phys: object.page(offset + (page - start)), prot, owned: false, cache: object.cache() })
        }
    })
}

/// Every frame the space owns goes back; its tables go with `UserTables`.
///
/// Not through [`AddressSpace::clear`]: no thread runs in a space that drops,
/// and a private frame is mapped in no other, so no CPU caches its leaves, no
/// futex waiter is keyed on its frames and no copy pins them.
impl Drop for AddressSpace {
    fn drop(&mut self) {
        assert!(!self.kernel, "space: the kernel's dropped");
        self.tables.edit(FLOOR, TOP, |_, leaf| matches!(leaf, Leaf::Mapped(Mapping { owned: false, .. })).then_some(Leaf::Empty));
        let mut freed = 0;
        self.tables.take_owned(FLOOR, TOP, |frames| {
            // SAFETY: the space's own frames, which nothing maps once it drops.
            unsafe { pmm::free_raw(frames) };
            freed += frames.len() as u64;
        });
        self.usage.freed(freed);
    }
}
