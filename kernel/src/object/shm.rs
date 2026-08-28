//! A region of memory more than one process can see.
//!
//! Holding a handle allows mapping it; giving one away is `SYS_HANDLE_SEND`.
//! Mappings are torn down when the last handle goes; pages are freed when
//! the last `Arc` goes, always later since a handle holds an `Arc`.

use alloc::sync::Arc;
use alloc::vec::Vec;

use toyos_abi::syscall::SyscallError;

use crate::mm::paging::{CachePolicy, Prot};
use crate::mm::{align_2m, pmm, Unmapped, PAGE_2M};
use crate::process::{PageTables, Pid};
use crate::sync::Lock;
use crate::{DirectMap, UserAddr};

use super::{KObjectVariant, ObjectCore, ZeroHandles};

/// Physical pages a region keeps alive; behind an `Arc` since one page set
/// can back several objects.
// Unread by design: the vector's job is only to stay alive.
#[expect(dead_code)]
pub struct Pages(Vec<pmm::PhysPage>);

impl Pages {
    pub fn new(pages: Vec<pmm::PhysPage>) -> Self {
        Self(pages)
    }
}

/// A physical range and the memory type every mapping of it must carry:
/// SDM Vol. 3A §11.12.4 forbids one physical page under two memory types.
#[derive(Clone)]
pub struct Region {
    pub phys: DirectMap,
    pub size: u64,
    pub cache: CachePolicy,
    /// The pages, when the kernel owns them; `None` for firmware's
    /// framebuffer or an MMIO aperture it does not own.
    #[expect(dead_code, reason = "the Arc is what keeps the pages alive; nothing reads it")]
    pub pages: Option<Arc<Pages>>,
}

impl Region {
    /// A placeholder for a driver struct built before its buffers exist:
    /// size zero maps nothing.
    pub fn empty() -> Self {
        Self { phys: DirectMap::from_phys(0), size: 0, cache: CachePolicy::DeferToMtrr, pages: None }
    }
}

pub struct SharedMemObject {
    pub(super) core: ObjectCore,
    region: Region,
    /// Where this region is mapped, per process; the zero-handle hook empties it.
    mapped_in: Lock<Vec<(Pid, PageTables, UserAddr)>>,
}

impl SharedMemObject {
    /// A region over memory somebody else owns: unmapped when the last
    /// handle goes, never freed here.
    pub fn over(region: Region) -> Arc<Self> {
        assert!(
            region.phys.phys() & (PAGE_2M - 1) == 0,
            "shm: {:#x} is not 2 MiB aligned",
            region.phys.phys(),
        );
        Arc::new(Self {
            core: Self::new_core(),
            region,
            mapped_in: Lock::new(Vec::new()),
        })
    }

    /// A fresh allocation, rounded up to whole 2 MiB pages; `InvalidArgument`
    /// for an unrepresentable size, `ResourceExhausted` when memory is short
    /// — no cap above that, since `alloc_contiguous` already refuses more
    /// than free physical memory.
    pub fn create(size: u64) -> Result<Arc<Self>, SyscallError> {
        if size == 0 || (size as usize).checked_add(PAGE_2M as usize - 1).is_none() {
            return Err(SyscallError::InvalidArgument);
        }
        let aligned = align_2m(size as usize);
        let pages = pmm::alloc_contiguous(aligned / PAGE_2M as usize, pmm::Category::SharedMemory)
            .ok_or(SyscallError::ResourceExhausted)?;
        let phys = DirectMap::from_phys(pages[0].direct_map().phys());
        Ok(Self::over(Region {
            phys,
            size: aligned as u64,
            cache: CachePolicy::DeferToMtrr,
            pages: Some(Arc::new(Pages(pages))),
        }))
    }

    pub fn size(&self) -> u64 {
        self.region.size
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
            self.mapped_in.lock().is_empty(),
            "shm koid {}: the region is mapped into a process already, so a kernel \
             write through the direct map is not exclusive",
            self.core.koid().raw(),
        );
        self.region.phys
    }

    /// Map into `pt`, or answer the address it is already mapped at;
    /// idempotent per process.
    pub fn map_into(&self, pid: Pid, pt: &PageTables) -> Result<u64, SyscallError> {
        let mut mapped = self.mapped_in.lock();
        if let Some((_, _, vaddr)) = mapped.iter().find(|(p, _, _)| *p == pid) {
            return Ok(vaddr.raw());
        }
        let (addr, _) = pt
            .lock()
            .alloc_and_map(self.region.phys.phys(), self.region.size, Prot::ReadWrite, self.region.cache)
            .ok_or(SyscallError::ResourceExhausted)?;
        // Logged only for a non-default policy: this process is the one
        // paying for it. Read back the installed policy, not the request,
        // so the line describes the mapping.
        if self.region.cache != CachePolicy::DeferToMtrr {
            let installed = pt.lock().user_policy(addr).expect("shm: just mapped");
            crate::log!(
                "shm: {:#x} mapped {:?} into pid {}",
                self.region.phys.phys(),
                installed,
                pid
            );
        }
        mapped.push((pid, Arc::clone(pt), addr));
        Ok(addr.raw())
    }

    /// Take this process's mapping away, if it has one; the caller owes a
    /// shootdown before the freed address can be reissued.
    #[must_use = "the caller owes a shootdown before the address can be reissued"]
    pub fn unmap_from(&self, pid: Pid) -> Option<Unmapped<()>> {
        let mut mapped = self.mapped_in.lock();
        let pos = mapped.iter().position(|(p, _, _)| *p == pid)?;
        let (_, pt, vaddr) = mapped.swap_remove(pos);
        pt.lock().free_and_unmap(vaddr);
        Some(Unmapped::new(()))
    }
}

/// Every mapping goes, flushed here; the pages do not, since a handle holds
/// an `Arc` and `Region::pages` frees them only when the last one drops.
impl ZeroHandles for SharedMemObject {
    fn on_zero_handles(&self) {
        let mapped = core::mem::take(&mut *self.mapped_in.lock());
        if mapped.is_empty() {
            return;
        }
        for (_, pt, vaddr) in &mapped {
            pt.lock().free_and_unmap(*vaddr);
        }
        drop(Unmapped::new(mapped));
    }
}

impl Drop for SharedMemObject {
    fn drop(&mut self) {
        debug_assert!(
            self.mapped_in.lock().is_empty(),
            "shm koid {} freed with a live mapping: the zero-handle hook did \
             not run, so the pages go back to the PMM under somebody's window",
            self.core.koid().raw(),
        );
    }
}
