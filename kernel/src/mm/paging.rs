// Page tables and address spaces: the only code that writes page table
// entries, for the kernel direct map and per-process user address spaces.
// A live-structure write's TLB invalidation is derived from the entry it
// replaced, never chosen by the caller, and discharges only on this CPU —
// `arch::tlb::shootdown` is the caller's job for the rest of the machine.
// No mapping here is global, which is what makes a single-address
// invalidation (INVPCID or INVLPG) complete.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use crate::hasher::HashMap;

use toyos_pcid::{Alloc, Pcid, PcidPool};

use super::{UserAddr, PAGE_2M};
use crate::arch::control_regs::PcidActive;
use crate::arch::cpu::Invpcid;
use crate::sync::Lock;
use crate::vma::{self, Region, RegionKind};
use crate::MemoryMapEntry;

const PAGE_PRESENT: u64 = 1 << 0;
const PAGE_WRITE: u64 = 1 << 1;
const PAGE_USER: u64 = 1 << 2;
const PAGE_WRITE_THROUGH: u64 = 1 << 3;
const PAGE_CACHE_DISABLE: u64 = 1 << 4;
/// Set by hardware on access or write; masked out when comparing entries.
const PAGE_ACCESSED: u64 = 1 << 5;
const PAGE_DIRTY: u64 = 1 << 6;
const PAGE_SIZE_BIT: u64 = 1 << 7;
/// In a 4 KiB PTE this bit is bit 7 instead, so flags can't cross a split unmoved.
const PAGE_PAT_2M: u64 = 1 << 12;
/// Not-executable only because `arch::control_regs` asserts `EFER.NXE` on every CPU.
const PAGE_NX: u64 = 1 << 63;
const ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;
const ADDR_MASK_2M: u64 = 0x000F_FFFF_FFE0_0000;

/// Every upper-level table entry's flags: present, writable, user.
const TABLE_FLAGS: u64 = PAGE_PRESENT | PAGE_WRITE | PAGE_USER;

/// 4 KiB pages in one 2 MiB page.
const PAGES_PER_2M: usize = (PAGE_2M / 4096) as usize;

/// What a user mapping may be used for: no variant is both writable and
/// executable, and every variant implies read since `PAGE_USER` grants it
/// unconditionally.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Prot {
    /// Read-only: neither writable nor executable.
    Read,
    /// Data: readable and writable, never executable.
    ReadWrite,
    /// Code. Never writable.
    ReadExec,
}

impl Prot {
    /// The permission bits a leaf entry carries; address and cache policy stay the caller's.
    fn leaf_bits(self) -> u64 {
        let common = PAGE_PRESENT | PAGE_USER;
        match self {
            Self::Read => common | PAGE_NX,
            Self::ReadWrite => common | PAGE_WRITE | PAGE_NX,
            Self::ReadExec => common,
        }
    }
}

/// What each 4 KiB page of a 2 MiB window may be used for: split because
/// `toyos-ld` can align a window across the end of `.text` and start of `.data`.
pub struct WindowProt([Prot; PAGES_PER_2M]);

impl WindowProt {
    /// A window whose pages all say the same thing.
    pub const fn uniform(prot: Prot) -> Self {
        Self([prot; PAGES_PER_2M])
    }

    /// Sets the 4 KiB page `offset` bytes in; an out-of-window offset panics.
    pub fn set(&mut self, offset: u64, prot: Prot) {
        self.0[(offset / 4096) as usize] = prot;
    }

    /// The one protection every page carries, or `None` where they disagree.
    fn agreed(&self) -> Option<Prot> {
        let first = self.0[0];
        self.0.iter().all(|&p| p == first).then_some(first)
    }
}

/// Which PAT entry a 2 MiB mapping selects, out of the three this kernel
/// ever writes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CachePolicy {
    /// PAT entry 0 (WB); the range's actual type is the MTRR's (SDM Vol. 3A Table 11-7).
    DeferToMtrr,
    /// PAT entry [`pat::UC_ENTRY`](crate::arch::pat::UC_ENTRY): UC under
    /// every MTRR type (Table 11-7), whatever firmware set or forgot.
    Uncacheable,
    /// PAT entry [`pat::WC_ENTRY`](crate::arch::pat::WC_ENTRY).
    WriteCombining,
}

impl CachePolicy {
    fn pde_bits(self) -> u64 {
        match self {
            Self::DeferToMtrr => 0,
            Self::Uncacheable => PAGE_CACHE_DISABLE | PAGE_WRITE_THROUGH,
            Self::WriteCombining => PAGE_PAT_2M,
        }
    }

    /// Any other combination is an entry this code never wrote.
    fn from_pde(pde: u64) -> Self {
        match (pde & PAGE_PAT_2M != 0, pde & (PAGE_CACHE_DISABLE | PAGE_WRITE_THROUGH)) {
            (false, 0) => Self::DeferToMtrr,
            (true, 0) => Self::WriteCombining,
            (false, low) if low == PAGE_CACHE_DISABLE | PAGE_WRITE_THROUGH => Self::Uncacheable,
            _ => panic!(
                "CachePolicy::from_pde: {pde:#x} selects a PAT entry outside 0, {} and {}",
                crate::arch::pat::UC_ENTRY,
                crate::arch::pat::WC_ENTRY
            ),
        }
    }
}

const _: () = assert!(
    crate::arch::pat::WC_ENTRY == 4,
    "WriteCombining sets the PAT bit and leaves PCD and PWT clear, which is entry 4",
);
const _: () = assert!(
    crate::arch::pat::UC_ENTRY == 3,
    "Uncacheable sets PCD and PWT and leaves the PAT bit clear, which is entry 3",
);

/// What an MMIO window may select — never PAT entry 0: device registers
/// deferred to firmware's MTRR coverage were cacheable wherever an MTRR was
/// missing, and this type removes that as a possibility.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MmioPolicy {
    /// Registers.
    Uncacheable,
    /// The scanout alone.
    WriteCombining,
}

impl MmioPolicy {
    fn cache(self) -> CachePolicy {
        match self {
            Self::Uncacheable => CachePolicy::Uncacheable,
            Self::WriteCombining => CachePolicy::WriteCombining,
        }
    }
}

/// A 4KB-aligned page of 512 entries, matching the hardware page table format.
#[repr(C, align(4096))]
struct PageTablePage([u64; 512]);

impl PageTablePage {
    fn phys(&self) -> u64 {
        super::DirectMap::phys_of(self)
    }

    /// # Safety
    /// `phys` must be a `PageTablePage` this module built and linked in;
    /// the returned reference must not outlive its address space.
    unsafe fn from_phys<'a>(phys: u64) -> &'a PageTablePage {
        &*super::DirectMap::from_phys(phys).as_ptr::<PageTablePage>()
    }

    /// # Safety
    /// Same as [`from_phys`], plus exclusivity: hold the only live reference.
    unsafe fn from_phys_mut<'a>(phys: u64) -> &'a mut PageTablePage {
        &mut *super::DirectMap::from_phys(phys).as_mut_ptr::<PageTablePage>()
    }

    fn child(&self, index: usize) -> Option<&PageTablePage> {
        let entry = self[index];
        if entry & PAGE_PRESENT != 0 {
            // SAFETY: PRESENT checked, so `entry & ADDR_MASK` names a table
            // this module installed (see `from_phys`).
            Some(unsafe { PageTablePage::from_phys(entry & ADDR_MASK) })
        } else {
            None
        }
    }

    fn child_mut(&mut self, index: usize) -> Option<&mut PageTablePage> {
        let entry = self[index];
        if entry & PAGE_PRESENT != 0 {
            // SAFETY: same as `child`; `&mut self` gives the exclusivity needed.
            Some(unsafe { PageTablePage::from_phys_mut(entry & ADDR_MASK) })
        } else {
            None
        }
    }

    /// Write one entry of a table the hardware may already be walking; what
    /// it owes the TLB is derived from the prior value.
    fn write(&mut self, idx: usize, va: u64, value: u64) -> Owed {
        Owed::of(core::mem::replace(&mut self.0[idx], value), va)
    }

    /// A present entry that named a page table (not a 2 MiB leaf) owes
    /// `Owed::Context`: one address can't invalidate its 512 leaves.
    fn write_pde(&mut self, idx: usize, va: u64, value: u64) -> Owed {
        let prior = core::mem::replace(&mut self.0[idx], value);
        if prior & PAGE_PRESENT != 0 && prior & PAGE_SIZE_BIT == 0 {
            Owed::Context
        } else {
            Owed::of(prior, va)
        }
    }

    /// Owes `Context` only if the widen actually changed a bit — a stale
    /// narrower entry can raise a spurious fault.
    fn widen(&mut self, idx: usize, flags: u64) -> Owed {
        let before = self.0[idx];
        self.0[idx] = before | flags;
        if self.0[idx] == before { Owed::Nothing } else { Owed::Context }
    }

    /// Not yet linked into a live structure, so nothing can be stale.
    fn init_entry(&mut self, idx: usize, value: u64) {
        self.0[idx] = value;
    }
}

/// What a write into a live paging structure owes this CPU's TLB, derived by
/// [`Owed::of`] from the prior entry alone.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[must_use = "an invalidation that is owed and not discharged is silent"]
enum Owed {
    /// The slot was not-present (SDM Vol. 3A §4.10.2.3: creates no TLB entry),
    /// or the write changed no bit.
    Nothing,
    /// One linear address, whose leaf changed under it.
    Address { va: u64, prior: u64 },
    /// Everything under an upper-level entry that widened, or a PDE that
    /// stopped naming its page table.
    Context,
}

impl Owed {
    /// The decision, as a function of the prior entry alone.
    fn of(prior: u64, va: u64) -> Self {
        if prior & PAGE_PRESENT == 0 {
            Self::Nothing
        } else {
            Self::Address { va, prior }
        }
    }

    /// `target` is the written space's CR3, never the live one: with PCID,
    /// `INVPCID` names its tag directly (SDM Vol. 3A §4.10.4.1), so writing a
    /// child's tables invalidates only the child; without PCID, a CPU holds
    /// entries only for its loaded CR3, so nothing is owed unless `target`
    /// is that CR3.
    fn discharge(self, target: Cr3) {
        match self {
            Self::Nothing => {}
            Self::Address { va, .. } => {
                if let Some(have) = pcid_active() {
                    crate::arch::cpu::invpcid(have, Invpcid::Address, target.pcid(), va);
                } else if Cr3::current().phys() == target.phys() {
                    crate::arch::cpu::invlpg(va);
                }
            }
            Self::Context => {
                if let Some(have) = pcid_active() {
                    crate::arch::cpu::invpcid(have, Invpcid::SinglePcid, target.pcid(), 0);
                } else if Cr3::current().phys() == target.phys() {
                    flush_tlb_all();
                }
            }
        }
    }

    /// Assert the slot the caller proved empty actually was: an empty slot
    /// owes nothing.
    fn expect_install(self, what: &str) {
        match self {
            Self::Nothing => {}
            Self::Address { va, prior } => {
                panic!("{what}: an install at {va:#x} found the present entry {prior:#x}")
            }
            Self::Context => panic!("{what}: an install widened an upper-level entry"),
        }
    }

    /// No-op: the caller already flushes more than this entry could owe (a
    /// shootdown, or a full local flush).
    fn subsumed_by_flush(self) {}
}

impl core::ops::Index<usize> for PageTablePage {
    type Output = u64;
    fn index(&self, idx: usize) -> &u64 {
        &self.0[idx]
    }
}

#[inline]
fn indices(addr: u64) -> (usize, usize, usize) {
    (
        ((addr >> 39) & 0x1FF) as usize,
        ((addr >> 30) & 0x1FF) as usize,
        ((addr >> 21) & 0x1FF) as usize,
    )
}

const CR3_NOFLUSH: u64 = 1 << 63;
const CR3_ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;

/// Whether `CR4.PCIDE` is declared; `Some` also serves as `cpu::invpcid`'s
/// `#UD` guard, so callers wanting only the bool ask `.is_some()`.
pub fn pcid_active() -> Option<PcidActive> {
    crate::arch::control_regs::PcidActive::ask()
}

/// Flush all TLB entries on this CPU, all PCIDs.
pub fn flush_tlb_all() {
    if let Some(have) = pcid_active() {
        crate::arch::cpu::invpcid(have, Invpcid::AllIncludingGlobal, 0, 0);
    } else {
        // SAFETY: the value is exactly what `read_cr3` just read on this
        // CPU, so writing it back is a valid CR3 write that only flushes
        // the TLB (there is no `PAGE_GLOBAL` in this kernel).
        unsafe {
            let cr3 = crate::arch::cpu::read_cr3();
            crate::arch::cpu::write_cr3(cr3);
        }
    }
}

/// CR3 register value: PML4 physical address | PCID.
#[derive(Clone, Copy)]
pub struct Cr3(u64);

impl Cr3 {
    pub fn current() -> Self {
        Self(crate::arch::cpu::read_cr3())
    }

    pub fn phys(self) -> u64 {
        self.0 & CR3_ADDR_MASK
    }
    pub fn pcid(self) -> u16 {
        (self.0 & 0xFFF) as u16
    }

    /// Sets NOFLUSH when PCID is active — sound only because a user tag is owned:
    /// no other live space holds it, and a reused one was flushed from every CPU
    /// before this space took it (`toyos_pcid`).
    /// # Safety
    /// The underlying page tables must be valid and live.
    pub unsafe fn activate(self) {
        if pcid_active().is_some() {
            crate::arch::cpu::write_cr3(self.0 | CR3_NOFLUSH);
        } else {
            crate::arch::cpu::write_cr3(self.0);
        }
    }

    /// Load CR3 with a flush; used during boot before PCID is enabled.
    /// # Safety
    /// The underlying page tables must be valid and live.
    pub unsafe fn load_flush(self) {
        crate::arch::cpu::write_cr3(self.0);
    }
}

/// The user PCID allocator: `toyos_pcid` owns the decision that no live tag is
/// reissued; here it is given the shootdown its [`Alloc::NeedsFlush`] asks for.
static PCID_POOL: Lock<PcidPool> = Lock::new(PcidPool::new());

/// A user tag owned for one address space's life; its drop returns the tag to
/// the pool. Non-`Copy`, so no second live space can name it.
struct PcidGuard(Pcid);

impl Drop for PcidGuard {
    fn drop(&mut self) {
        PCID_POOL.lock().free(self.0);
    }
}

/// Which tag an address space carries, and who returns it.
enum PcidHandle {
    /// The reserved tag 0; the kernel space is leaked, so it is never returned.
    Kernel,
    /// A user tag, returned to the pool when this handle drops.
    User(PcidGuard),
}

impl PcidHandle {
    fn value(&self) -> u16 {
        match self {
            Self::Kernel => toyos_pcid::KERNEL_PCID,
            Self::User(g) => g.0.get(),
        }
    }
}

/// `None` when all 4095 tags are held by live spaces. The pool lock is held
/// across the reclaim shootdown — safe under it because a CPU spinning for the
/// lock polls shootdowns, and so that one CPU reclaims at a time.
fn alloc_pcid() -> Option<PcidGuard> {
    let mut pool = PCID_POOL.lock();
    loop {
        match pool.alloc() {
            Alloc::Ready(p) => return Some(PcidGuard(p)),
            Alloc::NeedsFlush => {
                crate::arch::tlb::shootdown(crate::arch::tlb::Origin::Pcid);
                pool.reclaim();
            }
            Alloc::Exhausted => return None,
        }
    }
}

/// PML4[0..255] user, PML4[256..511] kernel direct map (shared).
pub struct AddressSpace {
    root: Box<PageTablePage>,
    children: Vec<Box<PageTablePage>>,
    /// Physical data pages mapped into user space, keyed by physical address. Freed on drop.
    pages: HashMap<u64, super::pmm::PhysPage>,
    /// All virtual memory regions, keyed by start address.
    regions: BTreeMap<UserAddr, Region>,
    /// Owned for this space's life: dropping the space returns a user tag, so two
    /// live spaces can never share one.
    pcid: PcidHandle,
}

/// Needed because a *placed* mapping (`sys_mmap`'s FIXED arm) skips
/// `find_gap`'s implicit check.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Occupancy {
    /// Nothing is registered over any part of it.
    Free,
    /// One region covers it end for end, and that region is all it runs into.
    Whole,
    /// Part of a region, several regions, or one that merely starts here.
    Partial,
}

fn align_up_2m(v: u64) -> u64 {
    (v + PAGE_2M - 1) & !(PAGE_2M - 1)
}

impl AddressSpace {
    /// Create a new user address space with kernel entries shallow-copied, or
    /// `None` when every user PCID is held by a live space.
    pub fn new_user() -> Option<Self> {
        let pcid = alloc_pcid()?;
        let kernel_as = kernel().lock();
        let mut pml4 = Box::new(PageTablePage([0; 512]));

        for i in 256..512 {
            if kernel_as.root[i] & PAGE_PRESENT != 0 {
                pml4.init_entry(i, kernel_as.root[i]);
            }
        }

        Some(Self {
            root: pml4,
            children: Vec::new(),
            pages: HashMap::default(),
            regions: BTreeMap::new(),
            pcid: PcidHandle::User(pcid),
        })
    }

    pub fn cr3(&self) -> Cr3 {
        Cr3(self.root.phys() | self.pcid.value() as u64)
    }

    /// Empty PDE slots (aligned, asserted) mean nothing can be stale.
    pub fn map_range(
        &mut self,
        vaddr: UserAddr,
        phys: u64,
        size: u64,
        prot: Prot,
        cache: CachePolicy,
    ) {
        assert!(
            vaddr.raw() & (PAGE_2M - 1) == 0,
            "map_range: vaddr not 2MB-aligned"
        );
        assert!(
            phys & (PAGE_2M - 1) == 0,
            "map_range: phys {phys:#x} not 2MB-aligned"
        );
        let mut offset = 0u64;
        while offset < size {
            let va = vaddr.raw() + offset;
            let pa = phys + offset;
            let flags = prot.leaf_bits() | cache.pde_bits();
            let pd_idx = indices(va).2;
            let pd = self.ensure_table(va, TABLE_FLAGS);
            pd.write_pde(pd_idx, va, pa | flags | PAGE_SIZE_BIT)
                .expect_install("map_range");
            offset += PAGE_2M;
        }
    }

    /// Private: leaves a placed `mmap` invisible to the placement search
    /// unless paired with unregistering — [`free_and_unmap`](Self::free_and_unmap) does both.
    fn unmap_range(&mut self, vaddr: UserAddr, size: u64) {
        let mut offset = 0u64;
        while offset < size {
            self.unmap(UserAddr::new(vaddr.raw() + offset));
            offset += PAGE_2M;
        }
    }

    /// Discharges only in this address space — a caller whose mapping may be live elsewhere (e.g. `sys_dlopen`) must shoot down itself.
    pub fn remap(&mut self, vaddr: UserAddr, phys: u64, prot: Prot) {
        let va = vaddr.raw();
        assert!(
            va & (PAGE_2M - 1) == 0,
            "remap: vaddr {va:#x} not 2MB-aligned"
        );
        assert!(
            phys & (PAGE_2M - 1) == 0,
            "remap: phys {phys:#x} not 2MB-aligned"
        );

        let pd_idx = indices(va).2;
        let target = self.cr3();
        let pd = self.ensure_table(va, TABLE_FLAGS);
        pd.write_pde(pd_idx, va, phys | prot.leaf_bits() | PAGE_SIZE_BIT)
            .discharge(target);
    }

    /// A mixed window's page table is written before being linked, so nothing
    /// can be walking it while filled. Must not be called twice on one
    /// address — the second call's PDE orphans the first call's table.
    pub fn map_window(&mut self, vaddr: UserAddr, phys: u64, prot: &WindowProt) {
        if let Some(uniform) = prot.agreed() {
            self.remap(vaddr, phys, uniform);
            return;
        }
        let va = vaddr.raw();
        assert!(
            va & (PAGE_2M - 1) == 0,
            "map_window: vaddr {va:#x} not 2MB-aligned"
        );
        assert!(
            phys & (PAGE_2M - 1) == 0,
            "map_window: phys {phys:#x} not 2MB-aligned"
        );

        let mut table = Box::new(PageTablePage([0; 512]));
        for (i, &page_prot) in prot.0.iter().enumerate() {
            // No cache bits: `DeferToMtrr` is the zero pattern at both granularities.
            table.init_entry(i, (phys + i as u64 * 4096) | page_prot.leaf_bits());
        }
        let table_phys = table.phys();
        self.children.push(table);

        let pd_idx = indices(va).2;
        let target = self.cr3();
        let pd = self.ensure_table(va, TABLE_FLAGS);
        // No `Prot` here: `NX` would make the whole window non-executable
        // whatever the leaves say.
        pd.write_pde(pd_idx, va, table_phys | TABLE_FLAGS).discharge(target);
    }

    /// `false` leaves the mapping as found and `phys` the caller's to free.
    /// The check and the write are one critical section: the demand pager
    /// fills unlocked, so two threads can both see "unmapped" first, and the
    /// second must lose here rather than strand the winner's CPU translation.
    pub fn map_window_if_absent(
        &mut self,
        vaddr: UserAddr,
        phys: u64,
        prot: &WindowProt,
    ) -> bool {
        if self.translate(vaddr).is_some() {
            return false;
        }
        self.map_window(vaddr, phys, prot);
        true
    }

    /// Runs on every present entry, including shared pages this space does
    /// not own: unmapping ends any futex wait on the frame (its token is a
    /// physical address) before the frame can reach the PMM and be reused
    /// under a waiter's nose — a spurious wake is safe, a use-after-free isn't.
    pub fn unmap(&mut self, vaddr: UserAddr) {
        let va = vaddr.raw();
        assert!(
            va & (PAGE_2M - 1) == 0,
            "unmap: vaddr {va:#x} not 2MB-aligned"
        );

        let (pml4_idx, pdpt_idx, pd_idx) = indices(va);
        let target = self.cr3();

        if let Some(pdpt) = self.root.child_mut(pml4_idx) {
            if let Some(pd) = pdpt.child_mut(pdpt_idx) {
                let pde = pd[pd_idx];
                if pde & PAGE_PRESENT != 0 {
                    // A split window's PDE names a table; every entry in it
                    // addresses the same 2 MiB frame, so entry 0 names it.
                    let phys = if pde & PAGE_SIZE_BIT != 0 {
                        pde & ADDR_MASK_2M
                    } else {
                        // SAFETY: PRESENT and not-2-MiB, so this names a
                        // `map_window`-built table, dropped before `write_pde` touches the slot.
                        let table = unsafe { PageTablePage::from_phys(pde & ADDR_MASK) };
                        table[0] & ADDR_MASK_2M
                    };
                    // `write_pde`: a PDE naming a page table owes the context.
                    pd.write_pde(pd_idx, va, 0).discharge(target);
                    // Before the frame is freed, so waiters stay findable.
                    crate::sched::waitqs::revoke_futex_range(phys, PAGE_2M);
                    self.pages.remove(&phys);
                }
            }
        }
    }

    /// Checked here, not at the callers: a user space shallow-copies the
    /// kernel PML4 half, so a kernel address would otherwise walk to a writable kernel page.
    pub fn translate(&self, vaddr: UserAddr) -> Option<super::DirectMap> {
        let va = vaddr.raw();
        if !toyos_userbound::is_user_addr(va) {
            return None;
        }
        let (pml4_idx, pdpt_idx, pd_idx) = indices(va);
        let pdpt = self.root.child(pml4_idx)?;
        let pd = pdpt.child(pdpt_idx)?;
        let pde = pd[pd_idx];
        if pde & PAGE_PRESENT == 0 {
            return None;
        }
        if pde & PAGE_SIZE_BIT == 0 {
            // A window `map_window` split, so the leaf is one level down.
            let pt = pd.child(pd_idx)?;
            let pte = pt[((va >> 12) & 0x1FF) as usize];
            if pte & PAGE_PRESENT == 0 {
                return None;
            }
            return Some(super::DirectMap::from_phys((pte & ADDR_MASK) + (va & 0xFFF)));
        }
        let page_phys = pde & ADDR_MASK_2M;
        let offset = va & (PAGE_2M - 1);
        Some(super::DirectMap::from_phys(page_phys + offset))
    }

    /// Find a free gap of at least `size` bytes (2MB-aligned), searching top-down.
    fn find_gap(&self, size: u64) -> Option<UserAddr> {
        let aligned = align_up_2m(size);
        let total = aligned + vma::GUARD_SIZE;

        let mut top = vma::ALLOC_CEILING;
        for (&start, region) in self
            .regions
            .range(..UserAddr::new(vma::ALLOC_CEILING))
            .rev()
        {
            let region_end = align_up_2m(start.raw() + region.size);
            if region_end > top {
                top = start.raw();
                continue;
            }
            let gap = top - region_end;
            if gap >= total {
                return Some(UserAddr::new(top - total));
            }
            top = start.raw();
        }
        // Gap below all regions
        if top >= total + vma::alloc_floor() {
            return Some(UserAddr::new(top - total));
        }
        None
    }

    /// Allocate a virtual address range and register the region.
    pub fn alloc_region(&mut self, size: u64, kind: RegionKind) -> Option<UserAddr> {
        let aligned = align_up_2m(size);
        let addr = self.find_gap(aligned)?;
        self.regions.insert(addr, Region { size: aligned, kind });
        Some(addr)
    }

    /// A mixed-`Prot` image uses [`alloc_region`](Self::alloc_region) plus [`map_window`](Self::map_window) per 2 MiB instead.
    pub fn alloc_and_map(
        &mut self,
        phys: u64,
        size: u64,
        prot: Prot,
        cache: CachePolicy,
    ) -> Option<(UserAddr, u64)> {
        let aligned = align_up_2m(size);
        assert!(
            phys & (PAGE_2M - 1) == 0,
            "alloc_and_map: phys {phys:#x} not 2MB-aligned"
        );
        let addr = self.find_gap(aligned)?;
        self.regions.insert(
            addr,
            Region {
                size: aligned,
                kind: RegionKind::Mapped,
            },
        );
        self.map_range(addr, phys, aligned, prot, cache);
        Some((addr, aligned))
    }

    /// Free a previously allocated region and unmap it.
    pub fn free_and_unmap(&mut self, addr: UserAddr) -> Option<u64> {
        let size = self.regions.remove(&addr)?.size;
        self.unmap_range(addr, size);
        Some(size)
    }

    /// Insert a region at a specific address (for ELF segments, stack, etc.)
    pub fn insert_region(&mut self, addr: UserAddr, region: Region) {
        assert!(
            self.find_region(addr).is_none(),
            "insert_region: address {:#x} already occupied",
            addr.raw()
        );
        self.regions.insert(addr, region);
    }

    /// Find the region containing `addr`. Returns (start_addr, region).
    pub fn find_region(&self, addr: UserAddr) -> Option<(UserAddr, &Region)> {
        let (&start, region) = self.regions.range(..=addr).next_back()?;
        if addr.raw() < start.raw() + region.size {
            Some((start, region))
        } else {
            None
        }
    }

    /// The end is saturating so a caller's arithmetic cannot wrap into a smaller range.
    pub fn occupancy(&self, addr: UserAddr, size: u64) -> Occupancy {
        let end = UserAddr::new(addr.raw().saturating_add(size));
        let mut over = self.overlapping_regions(addr, end);
        let Some((&start, region)) = over.next() else {
            return Occupancy::Free;
        };
        if over.next().is_none() && start == addr && region.size == size {
            Occupancy::Whole
        } else {
            Occupancy::Partial
        }
    }

    /// Iterate all regions that overlap the range [start, end).
    pub fn overlapping_regions(
        &self,
        start: UserAddr,
        end: UserAddr,
    ) -> impl Iterator<Item = (&UserAddr, &Region)> {
        // Overlaps [start, end) iff s < end && s+n > start; `range(..end)` prunes the first half.
        self.regions
            .range(..end)
            .filter(move |(&s, r)| s.raw() + r.size > start.raw())
    }

    /// Private: not safe to use until every CPU is told — the free fn [`map_mmio`] is the whole operation.
    fn map_mmio(&mut self, phys: u64, size: u64, cache: CachePolicy) -> super::Mmio {
        let start = phys & !(PAGE_2M - 1);
        let end = (phys + size + PAGE_2M - 1) & !(PAGE_2M - 1);
        let mut cur = start;
        while cur < end {
            self.map_2m(cur, PAGE_PRESENT | PAGE_WRITE | cache.pde_bits());
            cur += PAGE_2M;
        }
        super::Mmio::new(super::DirectMap::from_phys(phys), size)
    }

    /// Read from the table rather than remembered, or `None` if unmapped.
    fn policy_at(&self, virt: u64) -> Option<CachePolicy> {
        let (pml4_idx, pdpt_idx, pd_idx) = indices(virt);
        let pde = self.root.child(pml4_idx)?.child(pdpt_idx)?[pd_idx];
        if pde & PAGE_PRESENT == 0 {
            return None;
        }
        if pde & PAGE_SIZE_BIT == 0 {
            // A split table: leaves have cache bits clear (`DeferToMtrr`),
            // asserted since the two granularities put the PAT bit at different offsets.
            let pte = self.root.child(pml4_idx)?.child(pdpt_idx)?.child(pd_idx)?
                [((virt >> 12) & 0x1FF) as usize];
            assert!(
                pte & (PAGE_CACHE_DISABLE | PAGE_WRITE_THROUGH | PAGE_SIZE_BIT) == 0,
                "policy_at: the 4 KiB entry {pte:#x} at {virt:#x} selects a PAT entry \
                 outside 0",
            );
            return Some(CachePolicy::DeferToMtrr);
        }
        Some(CachePolicy::from_pde(pde))
    }

    pub fn direct_map_policy(&self, phys: u64) -> Option<CachePolicy> {
        self.policy_at(super::DirectMap::from_phys(phys).as_ptr::<u8>() as u64)
    }

    pub fn user_policy(&self, addr: UserAddr) -> Option<CachePolicy> {
        self.policy_at(addr.raw())
    }

    /// Permanent: the caller must own `phys` for the machine's life, since
    /// handing the enclosing 2 MiB page back to the PMM would reissue memory with a hole.
    pub fn guard_4k(&mut self, phys: u64) {
        assert!(phys & 0xFFF == 0, "guard_4k: phys {phys:#x} not 4 KiB-aligned");
        let virt = super::DirectMap::from_phys(phys).as_ptr::<u8>() as u64;
        let (pml4_idx, pdpt_idx, pd_idx) = indices(virt);
        let pd_phys = {
            let pdpt = self.root.child(pml4_idx).expect("guard_4k: no PDPT over the direct map");
            let entry = pdpt[pdpt_idx];
            assert!(entry & PAGE_PRESENT != 0, "guard_4k: no PD over the direct map");
            entry & ADDR_MASK
        };
        // SAFETY: `pd_phys` names the direct map's PD, living forever;
        // `&mut self` here is `kernel().lock()`'s required exclusivity.
        let pd = unsafe { PageTablePage::from_phys_mut(pd_phys) };
        let pde = pd[pd_idx];
        assert!(pde & PAGE_PRESENT != 0, "guard_4k: {phys:#x} is not in the direct map");

        // Already split: an earlier guard in the same 2 MiB region.
        if pde & PAGE_SIZE_BIT != 0 {
            let base = pde & ADDR_MASK_2M;
            let flags = pde & !ADDR_MASK_2M & !PAGE_SIZE_BIT;
            assert!(
                flags & PAGE_PAT_2M == 0,
                "guard_4k: {phys:#x} carries a PAT bit that is an address bit in a 4 KiB PTE"
            );
            let mut pt = Box::new(PageTablePage([0; 512]));
            for i in 0..512 {
                pt.init_entry(i, (base + i as u64 * 4096) | flags);
            }
            let pt_phys = pt.phys();
            self.children.push(pt);
            // Covered by the flush below, wider than either write could owe.
            pd.write_pde(pd_idx, virt, pt_phys | PAGE_PRESENT | PAGE_WRITE)
                .subsumed_by_flush();
        }

        // SAFETY: `pd[pd_idx]` names a page table either way, permanently —
        // same lock exclusivity as above.
        let pt = unsafe { PageTablePage::from_phys_mut(pd[pd_idx] & ADDR_MASK) };
        let idx = ((phys >> 12) & 0x1FF) as usize;
        assert!(pt[idx] & PAGE_PRESENT != 0, "guard_4k: {phys:#x} is already unmapped");
        pt.write(idx, virt, 0).subsumed_by_flush();

        // Local only: the guard belongs to one CPU, so only it dereferences
        // the removed page; `alloc_idle_stack` runs on the BSP before an
        // AP's TLB exists, and a sibling's stale 2 MiB entry stays correct
        // for the other 511 pages it still maps unchanged.
        flush_tlb_all();
    }

    /// Replaces whatever is there — the boot map covers every physical
    /// address, so an MMIO window's target is pre-mapped by the time its
    /// driver asks; a page `guard_4k` already split must not reach here.
    fn map_2m(&mut self, phys: u64, flags: u64) {
        let virt = super::DirectMap::from_phys(phys).as_ptr::<u8>() as u64;
        let pd_idx = indices(virt).2;
        let pd = self.ensure_table(virt, flags);
        let entry = phys | flags | PAGE_SIZE_BIT;
        let existing = pd[pd_idx];
        assert!(
            existing & PAGE_PRESENT == 0
                || existing & !(PAGE_ACCESSED | PAGE_DIRTY) == entry
                || (existing & PAGE_SIZE_BIT != 0
                    && CachePolicy::from_pde(existing) == CachePolicy::DeferToMtrr),
            "map_2m: {phys:#x} is mapped {existing:#x} and cannot also be {entry:#x}"
        );
        // Neither caller wants the single address: [`map_mmio`] flushes every
        // CPU (type may have changed under a sibling); `init` has no TLB yet.
        pd.write_pde(pd_idx, virt, entry).subsumed_by_flush();
    }

    /// Masks `flags` to `TABLE_FLAGS`: bit 12 is a leaf's PAT bit but an
    /// upper entry's address bit, so leaf flags can't move a table 4 KiB.
    fn ensure_table(&mut self, va: u64, flags: u64) -> &mut PageTablePage {
        let flags = flags & TABLE_FLAGS;
        let (pml4_idx, pdpt_idx, _) = indices(va);
        let target = self.cr3();

        if self.root[pml4_idx] & PAGE_PRESENT == 0 {
            let child = Box::new(PageTablePage([0; 512]));
            self.root
                .write(pml4_idx, va, child.phys() | flags)
                .expect_install("ensure_table: pml4");
            self.children.push(child);
        } else {
            // Upper-level entries: widen only (OR), never narrow.
            self.root.widen(pml4_idx, flags).discharge(target);
        }

        // SAFETY: the branch above guaranteed PRESENT, so `& ADDR_MASK`
        // names a table this space owns for life; `&mut self` gives the
        // exclusivity `from_phys_mut` requires.
        let pdpt = unsafe { PageTablePage::from_phys_mut(self.root[pml4_idx] & ADDR_MASK) };

        if pdpt[pdpt_idx] & PAGE_PRESENT == 0 {
            let child = Box::new(PageTablePage([0; 512]));
            pdpt.write(pdpt_idx, va, child.phys() | flags)
                .expect_install("ensure_table: pdpt");
            self.children.push(child);
        } else {
            pdpt.widen(pdpt_idx, flags).discharge(target);
        }

        // SAFETY: same argument as `pdpt` above, one level down.
        unsafe { PageTablePage::from_phys_mut(pdpt[pdpt_idx] & ADDR_MASK) }
    }
}

const MIN_PHYS_MAP: u64 = 4 * 1024 * 1024 * 1024;

/// `Arc<Lock<AddressSpace>>`, not `Lock<Option<_>>`: a kernel thread names it
/// as `KernelPayload.address_space` with no second answer. Leaked, since the
/// kernel address space outlives every task by construction.
static KERNEL: core::sync::atomic::AtomicPtr<alloc::sync::Arc<Lock<AddressSpace>>> =
    core::sync::atomic::AtomicPtr::new(core::ptr::null_mut());

/// Kernel CR3, cached for lock-free access from panic/crash paths.
static KERNEL_CR3: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// The kernel address space. Mapped once at boot, lives forever.
pub fn kernel() -> &'static alloc::sync::Arc<Lock<AddressSpace>> {
    let ptr = KERNEL.load(core::sync::atomic::Ordering::Acquire);
    assert!(!ptr.is_null(), "paging not initialized");
    // SAFETY: written once in `init` with the `Release` this `Acquire` pairs
    // with, never cleared, so the pointer is live for the machine's life.
    unsafe { &*ptr }
}

/// Kernel CR3. Lock-free — safe to call from panic context.
pub fn kernel_cr3() -> Cr3 {
    Cr3(KERNEL_CR3.load(core::sync::atomic::Ordering::Relaxed))
}

/// Leave whatever user address space is current for the kernel's own. Safe,
/// unlike [`Cr3::activate`]: the kernel tables never move, so there's no
/// argument left for a caller to get wrong. Outgoing user mappings stop
/// being addressable at this line.
pub fn activate_kernel() {
    // SAFETY: `KERNEL_CR3` names the boot-built tables, mapping the code and
    // stack this call returns onto.
    unsafe { kernel_cr3().activate() };
}

/// [`activate_kernel`] for a CPU without `CR4.PCIDE` yet: `activate` sets the
/// reserved `NOFLUSH` bit once the *machine* declares PCIDE, which an AP
/// reaches before setting it on itself.
pub fn load_kernel_flush() {
    // SAFETY: same as `activate_kernel` above.
    unsafe { kernel_cr3().load_flush() };
}

/// Free function (not a method): the lock and the shootdown are separate
/// statements. Not optional — `map_2m` may change memory type under a
/// sibling's stale entry, which is SDM Vol. 3A §11.12.4 undefined behaviour.
pub fn map_mmio(phys: u64, size: u64, policy: MmioPolicy) -> super::Mmio {
    let mmio = kernel().lock().map_mmio(phys, size, policy.cache());
    crate::arch::tlb::shootdown(crate::arch::tlb::Origin::Mmio);
    // Read back off the table and logged beside firmware's MTRR verdict: the
    // boot's own evidence that no register window trusts firmware.
    let installed =
        kernel().lock().direct_map_policy(phys).expect("map_mmio: the window was just mapped");
    assert!(installed == policy.cache(), "map_mmio: {phys:#x} installed {installed:?}");
    crate::log!(
        "mmio: {phys:#x}+{size:#x} PAT {installed:?} (MTRR {})",
        crate::arch::mtrr::range_type(phys, size).name()
    );
    mmio
}


/// Take the 4 KiB page holding `addr` out of the kernel direct map; `addr`'s
/// page must be owned by the caller forever (see [`AddressSpace::guard_4k`]).
pub fn guard_kernel_page(addr: u64) {
    assert!(super::is_kernel_addr(addr), "guard_kernel_page: {addr:#x} is not a kernel address");
    kernel().lock().guard_4k(super::DirectMap::phys_of(addr as *const u8));
}

/// Build kernel page tables: map all physical memory in the high half using 2MB large pages.
pub(super) fn init(memory_map: &[MemoryMapEntry]) {
    let mut max_addr: u64 = MIN_PHYS_MAP;
    for entry in memory_map {
        if entry.end > max_addr {
            max_addr = entry.end;
        }
    }
    max_addr = (max_addr + PAGE_2M - 1) & !(PAGE_2M - 1);

    let mut kernel = AddressSpace {
        root: Box::new(PageTablePage([0; 512])),
        children: Vec::new(),
        pages: HashMap::default(),
        regions: BTreeMap::new(),
        pcid: PcidHandle::Kernel,
    };

    let mut addr: u64 = 0;
    while addr < max_addr {
        kernel.map_2m(addr, PAGE_PRESENT | PAGE_WRITE);
        addr += PAGE_2M;
    }

    let cr3 = kernel.cr3();
    KERNEL_CR3.store(cr3.0, core::sync::atomic::Ordering::Release);
    // Leaked, and `Release` after the space is built: see [`KERNEL`].
    let published: &'static alloc::sync::Arc<Lock<AddressSpace>> = Box::leak(Box::new(
        alloc::sync::Arc::new(Lock::new(kernel)),
    ));
    KERNEL.store(
        published as *const _ as *mut _,
        core::sync::atomic::Ordering::Release,
    );
    // SAFETY: `cr3` names the space this function just finished building via
    // the `map_2m` loop above; nothing has run under it yet, so live reduces
    // to self-consistent.
    unsafe {
        cr3.load_flush();
    }
}

fn has(entry: u64, flag: u64) -> u8 {
    if entry & flag != 0 {
        1
    } else {
        0
    }
}

/// The *currently loaded* CR3, not `kernel_cr3()` (a panic can run on a user
/// space); lock-free and silent, for the panic path to prove a mapping
/// before writing through it.
pub fn present_in_current_cr3(addr: u64) -> bool {
    // SAFETY: `Cr3::current().phys()` names the table this CPU runs under
    // right now, so it can't be freed meanwhile. No lock: each entry read is
    // one aligned `u64`, atomic at the hardware level, so no read is torn.
    let mut table = unsafe { PageTablePage::from_phys(Cr3::current().phys()) };
    for level in 0..3 {
        let entry = table[((addr >> (39 - level * 9)) & 0x1FF) as usize];
        if entry & PAGE_PRESENT == 0 {
            return false;
        }
        if level > 0 && entry & PAGE_SIZE_BIT != 0 {
            return true;
        }
        // SAFETY: PRESENT just checked; same argument as above the loop — a
        // present entry under the current CR3 names a live table.
        table = unsafe { PageTablePage::from_phys(entry & ADDR_MASK) };
    }
    table[((addr >> 12) & 0x1FF) as usize] & PAGE_PRESENT != 0
}

/// Dump page table entries for an address. Lock-free for crash safety.
pub fn debug_page_walk(addr: u64) {
    let cr3 = Cr3::current();
    // SAFETY: same argument as `present_in_current_cr3` above.
    let pml4 = unsafe { PageTablePage::from_phys(cr3.phys()) };
    let (pml4_idx, pdpt_idx, pd_idx) = indices(addr);
    let pt_idx = ((addr >> 12) & 0x1FF) as usize;

    log!(
        "  Page walk for {:#x} [PML4={:#x} PCID={} PML4[{}] PDPT[{}] PD[{}] PT[{}]]:",
        addr,
        cr3.phys(),
        cr3.pcid(),
        pml4_idx,
        pdpt_idx,
        pd_idx,
        pt_idx
    );

    let pml4e = pml4[pml4_idx];
    log!(
        "    PML4E: {:#018x} P={} W={} U={}",
        pml4e,
        has(pml4e, PAGE_PRESENT),
        has(pml4e, PAGE_WRITE),
        has(pml4e, PAGE_USER)
    );
    if pml4e & PAGE_PRESENT == 0 {
        return;
    }

    // SAFETY: PRESENT checked; same argument as `pml4` above.
    let pdpt = unsafe { PageTablePage::from_phys(pml4e & ADDR_MASK) };
    let pdpte = pdpt[pdpt_idx];
    log!(
        "    PDPTE: {:#018x} P={} W={} U={}",
        pdpte,
        has(pdpte, PAGE_PRESENT),
        has(pdpte, PAGE_WRITE),
        has(pdpte, PAGE_USER)
    );
    if pdpte & PAGE_PRESENT == 0 {
        return;
    }

    // SAFETY: PRESENT checked; same reasoning as `pdpt` above, one level down.
    let pd = unsafe { PageTablePage::from_phys(pdpte & ADDR_MASK) };
    let pde = pd[pd_idx];
    log!(
        "    PDE:   {:#018x} P={} W={} U={} PS={}",
        pde,
        has(pde, PAGE_PRESENT),
        has(pde, PAGE_WRITE),
        has(pde, PAGE_USER),
        has(pde, PAGE_SIZE_BIT)
    );
    if pde & PAGE_PRESENT == 0 {
        return;
    }
    if pde & PAGE_SIZE_BIT != 0 {
        log!("    -> 2MB large page at {:#x}", pde & ADDR_MASK_2M);
        return;
    }

    // SAFETY: PRESENT checked and the 2 MiB branch already returned, so
    // this is a table `map_window`/`guard_4k` built; same reasoning as above.
    let pt = unsafe { PageTablePage::from_phys(pde & ADDR_MASK) };
    let pte = pt[pt_idx];
    log!(
        "    PTE:   {:#018x} P={} W={} U={}",
        pte,
        has(pte, PAGE_PRESENT),
        has(pte, PAGE_WRITE),
        has(pte, PAGE_USER)
    );
    if pte & PAGE_PRESENT == 0 {
        return;
    }
    log!("    -> 4KB page at {:#x}", pte & ADDR_MASK);
}
