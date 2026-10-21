//! A region of memory more than one process can see.
//!
//! Holding a handle allows mapping it; giving one away is `SYS_MESSAGE_SEND`.
//! The handle count owns the mappings and the `Arc` count owns the pages: the
//! last handle takes the list of mappings away for good and tears each one
//! down, and the pages are freed when the last `Arc` goes, always later since
//! a handle holds an `Arc`. An `Arc` a syscall cloned out of its table can
//! outlive the last handle, so a map through one finds no list and is refused
//! under the lock the teardown took it with.
//!
//! **A mapping does not keep its process's address space**: the list holds it
//! weakly, so a process that ended while another still holds the region has
//! its space, and everything the space holds, freed with it; its entry is dead
//! and is passed over.

use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;

use toyos_abi::syscall::SyscallError;

use crate::mm::policy::{CachePolicy, Prot};
use crate::mm::paging::Detached;
use crate::mm::{align_2m_checked, pmm, Unmapped, PAGE_2M, PAGE_SIZE};
use crate::process::{PageTables, Pid};
use crate::sync::Lock;
use crate::{DirectMap, UserAddr};

use super::{Held, KObjectVariant, ObjectCore, ZeroHandles};

// The ABI's word for what `create` rounds to, which a receiver checks a lent region's length against.
const _: () = assert!(toyos_abi::syscall::SHM_PAGE as u64 == PAGE_2M);

/// Physical pages a region keeps alive; behind an `Arc` since one page set
/// can back several objects.
// Unread by design: the vector's job is only to stay alive.
#[expect(dead_code)]
pub struct Pages(Vec<pmm::PhysPage>);

/// A physical range and the memory type every mapping of it must carry:
/// SDM Vol. 3A §11.12.4 forbids one physical page under two memory types.
#[derive(Clone)]
pub struct Region {
    pub phys: DirectMap,
    pub size: u64,
    pub cache: CachePolicy,
    pub memory: Memory,
}

/// Whose memory a region is.
#[derive(Clone)]
pub enum Memory {
    /// Pages the kernel allocated, freed with the last `Arc`, which the
    /// field only keeps alive.
    Owned { _pages: Arc<Pages> },
    /// A payload the loader verified and handed over, `len` bytes of the
    /// region's pages: kept for the machine's life and written by nobody, so
    /// every mapping of it is read-only (`crate::bootimage`).
    Boot { len: u64 },
    /// Firmware's framebuffer or an MMIO aperture the kernel does not own.
    Foreign,
}

pub struct SharedMemObject {
    pub(super) core: ObjectCore,
    region: Region,
    /// Where this region is mapped, per process; released by the zero-handle hook.
    mapped_in: Held<Vec<Mapping>>,
}

/// One process's mapping of a region: dead once its address space is gone.
struct Mapping {
    pid: Pid,
    space: Weak<Lock<crate::mm::paging::AddressSpace>>,
    at: UserAddr,
}

impl Mapping {
    fn live(&self) -> Option<PageTables> {
        self.space.upgrade()
    }
}

impl SharedMemObject {
    /// A region over memory somebody else owns, whole 4 KiB pages: unmapped
    /// when the last handle goes, never freed here.
    pub fn over(region: Region) -> Arc<Self> {
        assert!(
            (region.phys.phys() | region.size) & (PAGE_SIZE - 1) == 0,
            "shm: {:#x} bytes at {:#x} are not whole 4 KiB pages",
            region.size,
            region.phys.phys(),
        );
        Arc::new(Self {
            core: Self::new_core(),
            region,
            mapped_in: Held::new(Vec::new()),
        })
    }

    /// A fresh allocation, rounded up to whole 2 MiB pages; `InvalidArgument`
    /// for an unrepresentable size, `ResourceExhausted` when memory is short
    /// — no cap above that, since `alloc_contiguous` already refuses more
    /// than free physical memory.
    pub fn create(size: u64) -> Result<Arc<Self>, SyscallError> {
        if size == 0 {
            return Err(SyscallError::InvalidArgument);
        }
        // `SYS_SHM_CREATE`'s length, straight from a register: the rounding
        // itself is the checked sum.
        let aligned = align_2m_checked(size).ok_or(SyscallError::InvalidArgument)?;
        let pages = pmm::alloc_contiguous((aligned / PAGE_2M) as usize)
            .ok_or(SyscallError::ResourceExhausted)?;
        let phys = DirectMap::from_phys(pages[0].direct_map().phys());
        Ok(Self::over(Region {
            phys,
            size: aligned,
            cache: CachePolicy::Normal,
            memory: Memory::Owned { _pages: Arc::new(Pages(pages)) },
        }))
    }

    pub fn size(&self) -> u64 {
        self.region.size
    }

    /// The physical run and its length, for a region that is ordinary memory
    /// this kernel allocated and owns — the only kind a device may be lent
    /// (`pcidev::dma_map`). `None` for anything else: a BAR window aimed into
    /// a second device's domain would be one device reaching another's
    /// registers.
    pub fn ram(&self) -> Option<(u64, u64)> {
        (matches!(self.region.memory, Memory::Owned { .. }) && self.region.cache == CachePolicy::Normal)
            .then(|| (self.region.phys.phys(), self.region.size))
    }

    /// The payload's length, for a region the loader handed over: memory
    /// nobody writes.
    pub fn immutable(&self) -> Option<u64> {
        match self.region.memory {
            Memory::Boot { len } => Some(len),
            Memory::Owned { .. } | Memory::Foreign => None,
        }
    }

    /// Whether a program may be paged from it: ordinary memory, never a
    /// device's, whose read is a device access.
    pub fn is_ordinary(&self) -> bool {
        self.ram().is_some() || self.immutable().is_some()
    }

    /// The kernel's own view of the pages, through the direct map; a mapped
    /// region is memory two writers can share — see `inbox.rs`.
    pub fn phys(&self) -> DirectMap {
        self.region.phys
    }

    /// The same address, before anybody else can see the pages: asserts the
    /// region is mapped nowhere yet, since after `map_into` a sibling thread
    /// could otherwise write the same bytes while the kernel is still initialising.
    pub fn phys_before_mapping(&self) -> DirectMap {
        assert!(
            self.mapped_nowhere(),
            "shm koid {}: the region is mapped into a process already, so a kernel \
             write through the direct map is not exclusive",
            self.core.koid().raw(),
        );
        self.region.phys
    }

    fn mapped_nowhere(&self) -> bool {
        self.mapped_in.with(|mapped| mapped.iter().all(|m| m.space.strong_count() == 0)).unwrap_or(true)
    }

    /// Map into `pt`, or answer the address it is already mapped at;
    /// idempotent per process. `Gone` once the last handle has gone.
    pub fn map_into(&self, pid: Pid, pt: &PageTables) -> Result<u64, SyscallError> {
        let map = |mapped: &mut Vec<Mapping>| {
            // A dead entry's pid may be this process's, reused.
            mapped.retain(|m| m.space.strong_count() > 0);
            if let Some(m) = mapped.iter().find(|m| m.space.as_ptr() == Arc::as_ptr(pt)) {
                return Ok(m.at.raw());
            }
            let prot = match self.region.memory {
                Memory::Boot { .. } => Prot::Read,
                Memory::Owned { .. } | Memory::Foreign => Prot::ReadWrite,
            };
            let (addr, _) = pt
                .lock()
                .alloc_and_map(self.region.phys.phys(), self.region.size, prot, self.region.cache)
                .ok_or(SyscallError::ResourceExhausted)?;
            // Logged only for a non-default policy: this process is the one
            // paying for it. Read back the installed policy, not the request,
            // so the line describes the mapping.
            if self.region.cache != CachePolicy::Normal {
                let installed = pt.lock().user_policy(addr).expect("shm: just mapped");
                crate::log!(
                    "shm: {:#x} mapped {:?} into pid {}",
                    self.region.phys.phys(),
                    installed,
                    pid
                );
            }
            mapped.push(Mapping { pid, space: Arc::downgrade(pt), at: addr });
            Ok(addr.raw())
        };
        self.mapped_in.with_mut(map).unwrap_or(Err(SyscallError::Gone))
    }

    /// Take this process's mapping away, if it has one; the caller owes a
    /// shootdown before the freed address can be reissued.
    #[must_use = "the caller owes a shootdown before the address can be reissued"]
    pub fn unmap_from(&self, pid: Pid) -> Option<Unmapped<Option<Detached>>> {
        self.mapped_in.with_mut(|mapped| {
            mapped.retain(|m| m.space.strong_count() > 0);
            let pos = mapped.iter().position(|m| m.pid == pid)?;
            let m = mapped.swap_remove(pos);
            let detached = m.live().and_then(|pt| pt.lock().free_and_unmap(m.at));
            Some(Unmapped::new(detached))
        })?
    }
}

/// Every mapping goes, flushed here, and with the list gone no later map can
/// add one; the pages do not, since a handle holds an `Arc` and
/// [`Memory::Owned`] frees them only when the last one drops.
impl ZeroHandles for SharedMemObject {
    fn on_zero_handles(&self) {
        let mapped = self.mapped_in.take().expect("the zero-handle hook runs once");
        if mapped.is_empty() {
            return;
        }
        let detached: Vec<_> =
            mapped.iter().filter_map(|m| m.live().and_then(|pt| pt.lock().free_and_unmap(m.at))).collect();
        drop(Unmapped::new((mapped, detached)));
    }
}

impl Drop for SharedMemObject {
    fn drop(&mut self) {
        debug_assert!(
            self.mapped_nowhere(),
            "shm koid {} freed with a live mapping, so the pages go back to the \
             PMM under somebody's window",
            self.core.koid().raw(),
        );
    }
}
