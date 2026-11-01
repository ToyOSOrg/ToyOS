// Page tables and address spaces: the only code that writes page table
// entries, for the kernel direct map and per-process user address spaces.
// A live-structure write's TLB invalidation is derived from the entry it
// replaced, never chosen by the caller, and discharges only on this CPU —
// `arch::tlb::shootdown` is the caller's job for the rest of the machine.
// A leaf is global if and only if it is the same in every root that will ever
// be loaded, which is the kernel half and only [`kernel_leaf`] writes it; a
// mapping that differs between roots needs a constructor of its own. No
// kernel leaf is both writable and executable, and only the image's text is
// executable (`mm::audit_wx`).

use crate::log;
use alloc::sync::Arc;
use alloc::vec::Vec;
use kernel::sched::job::Job;

use kernel::pcid::{Alloc, Pcid, PcidPool};

use crate::mm::PAGE_2M;
use crate::mm::policy::{CachePolicy, MmioPolicy, Prot};
use crate::arch::control_regs::PcidActive;
use crate::arch::cpu::Invpcid;
use crate::sync::Lock;
use toyos_bootmap::ROOT_HIGH_HALF;
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
/// Cached across every `CR3` and every PCID, because `arch::control_regs`
/// declares `CR4.PGE`; the same bit in a 2 MiB leaf and a 4 KiB one.
const PAGE_GLOBAL: u64 = 1 << 8;
/// In a 4 KiB PTE this bit is bit 7 instead, so flags can't cross a split unmoved.
const PAGE_PAT_2M: u64 = 1 << 12;
/// The PAT bit of a 4 KiB PTE, where a PDE has its size bit.
const PAGE_PAT_4K: u64 = 1 << 7;
/// Not-executable only because `arch::control_regs` sets `EFER.NXE` on every
/// CPU before it loads these tables.
const PAGE_NX: u64 = 1 << 63;
const ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;
const ADDR_MASK_2M: u64 = 0x000F_FFFF_FFE0_0000;
const PAGE_4K: u64 = crate::mm::PAGE_SIZE;

/// Every upper-level table entry's flags: present, writable, user.
const TABLE_FLAGS: u64 = PAGE_PRESENT | PAGE_WRITE | PAGE_USER;

impl Prot {
    /// The permission bits a user leaf carries; address and cache policy stay the caller's.
    fn leaf_bits(self) -> u64 {
        let common = PAGE_PRESENT | PAGE_USER;
        match self {
            Self::Read => common | PAGE_NX,
            Self::ReadWrite => common | PAGE_WRITE | PAGE_NX,
            Self::ReadExec => common,
        }
    }

    /// The permission bits a kernel leaf carries. A supervisor write to a
    /// read-only one faults because `CR0.WP` is declared (SDM Vol. 3A §4.6.1).
    fn kernel_bits(self) -> u64 {
        match self {
            Self::Read => PAGE_NX,
            Self::ReadWrite => PAGE_WRITE | PAGE_NX,
            Self::ReadExec => 0,
        }
    }
}

/// Every bit of a kernel-half leaf but its address and a 2 MiB leaf's size
/// bit: the one writer of [`PAGE_GLOBAL`]. Its PAT bit is a 2 MiB leaf's (bit
/// 12; a 4 KiB one's is bit 7), so a 4 KiB leaf takes it only for `Normal`,
/// which sets neither.
fn kernel_leaf(cache: CachePolicy, prot: Prot) -> u64 {
    PAGE_PRESENT | PAGE_GLOBAL | prot.kernel_bits() | cache.pde_bits()
}

/// Every upper-level entry over the kernel half: its leaves decide.
const KERNEL_TABLE: u64 = PAGE_PRESENT | PAGE_WRITE;

/// Which PAT entry a 2 MiB mapping selects: `Normal` is entry 0 (WB), whose
/// type for a range is the MTRR's (SDM Vol. 3A Table 11-7); `Uncacheable` is
/// [`pat::UC_ENTRY`](crate::arch::pat::UC_ENTRY), UC under every MTRR type;
/// `WriteCombining` is [`pat::WC_ENTRY`](crate::arch::pat::WC_ENTRY).
impl CachePolicy {
    fn pde_bits(self) -> u64 {
        self.bits(PAGE_PAT_2M)
    }

    fn pte_bits(self) -> u64 {
        self.bits(PAGE_PAT_4K)
    }

    fn bits(self, pat: u64) -> u64 {
        match self {
            Self::Normal => 0,
            Self::Uncacheable => PAGE_CACHE_DISABLE | PAGE_WRITE_THROUGH,
            Self::WriteCombining => pat,
        }
    }

    fn from_pde(pde: u64) -> Self {
        Self::from_bits(pde, PAGE_PAT_2M)
    }

    fn from_pte(pte: u64) -> Self {
        Self::from_bits(pte, PAGE_PAT_4K)
    }

    /// Any other combination is an entry this code never wrote.
    fn from_bits(entry: u64, pat: u64) -> Self {
        match (entry & pat != 0, entry & (PAGE_CACHE_DISABLE | PAGE_WRITE_THROUGH)) {
            (false, 0) => Self::Normal,
            (true, 0) => Self::WriteCombining,
            (false, low) if low == PAGE_CACHE_DISABLE | PAGE_WRITE_THROUGH => Self::Uncacheable,
            _ => panic!(
                "CachePolicy::from_bits: {entry:#x} selects a PAT entry outside 0, {} and {}",
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

/// A 4KB-aligned page of 512 entries, matching the hardware page table format.
#[repr(C, align(4096))]
struct PageTablePage([u64; 512]);

impl PageTablePage {
    /// An empty table of the kernel's in a frame of its own, owned from here
    /// by the entry that links it.
    fn new() -> &'static mut PageTablePage {
        let frame = crate::mm::pmm::alloc_frame().expect("paging: no frame for a page table");
        // SAFETY: a zeroed frame just allocated, so a table of empty entries
        // nothing else names.
        unsafe { Self::from_phys_mut(frame.into_raw()) }
    }

    /// A table of a user space, counted to `job`, or `None` where taking a
    /// frame would cut into what is held back (`pmm::alloc_table_frame`).
    fn try_new(job: &Job) -> Option<&'static mut PageTablePage> {
        let phys = crate::mm::pmm::alloc_table_frame(job)?;
        // SAFETY: a zeroed frame just allocated, so a table of empty entries
        // nothing else names.
        Some(unsafe { Self::from_phys_mut(phys) })
    }

    fn phys(&self) -> u64 {
        crate::mm::DirectMap::phys_of(self)
    }

    /// # Safety
    /// `phys` must be a `PageTablePage` this module built and linked in;
    /// the returned reference must not outlive its address space.
    unsafe fn from_phys<'a>(phys: u64) -> &'a PageTablePage {
        &*crate::mm::DirectMap::from_phys(phys).as_ptr::<PageTablePage>()
    }

    /// # Safety
    /// Same as [`from_phys`], plus exclusivity: hold the only live reference.
    unsafe fn from_phys_mut<'a>(phys: u64) -> &'a mut PageTablePage {
        &mut *crate::mm::DirectMap::from_phys(phys).as_mut_ptr::<PageTablePage>()
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
            Owed::Context { va }
        } else {
            Owed::of(prior, va)
        }
    }

    /// Owes `Context` only if the widen actually changed a bit — a stale
    /// narrower entry can raise a spurious fault.
    fn widen(&mut self, idx: usize, va: u64, flags: u64) -> Owed {
        let before = self.0[idx];
        self.0[idx] = before | flags;
        if self.0[idx] == before { Owed::Nothing } else { Owed::Context { va } }
    }

    /// One aligned store of an entry a walker may be reading, which the
    /// compiler neither splits nor merges.
    fn store(&mut self, idx: usize, value: u64) {
        // SAFETY: an entry of this table, aligned as every `u64` of it is.
        unsafe { core::ptr::write_volatile(&raw mut self.0[idx], value) };
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
    /// stopped naming its page table; `va` is an address under it.
    Context { va: u64 },
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
    /// is that CR3. A global translation is the exception to both: it is held
    /// under every CR3 and every PCID, and neither `INVPCID` type 0 or 1 nor
    /// a CR3 write drops it.
    fn discharge(self, target: Cr3) {
        match self {
            Self::Nothing => {}
            // With PCID, `INVPCID` type 2: Intel's ADL063 and RPL043 let an
            // `INVLPG` keep the global translation in every PCID but the current
            // one, and name type 2 "instead of INVLPG" as the software workaround.
            Self::Address { va, prior } if prior & PAGE_GLOBAL != 0 => match pcid_active() {
                Some(have) => crate::arch::cpu::invpcid(have, Invpcid::AllIncludingGlobal, 0, 0),
                None => crate::arch::cpu::invlpg(va),
            },
            Self::Address { va, .. } => {
                if let Some(have) = pcid_active() {
                    crate::arch::cpu::invpcid(have, Invpcid::Address, target.pcid(), va);
                } else if Cr3::current().phys() == target.phys() {
                    crate::arch::cpu::invlpg(va);
                }
            }
            // Every kernel-half leaf is global.
            Self::Context { va } if crate::mm::is_kernel_addr(va) => flush_tlb_all(),
            Self::Context { .. } => {
                if let Some(have) = pcid_active() {
                    crate::arch::cpu::invpcid(have, Invpcid::SinglePcid, target.pcid(), 0);
                } else if Cr3::current().phys() == target.phys() {
                    flush_tlb_user();
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
            Self::Context { .. } => panic!("{what}: an install widened an upper-level entry"),
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

/// Flush all TLB entries on this CPU, all PCIDs, global ones included: what a
/// kernel-half change owes.
pub fn flush_tlb_all() {
    if let Some(have) = pcid_active() {
        crate::arch::cpu::invpcid(have, Invpcid::AllIncludingGlobal, 0, 0);
    } else {
        crate::arch::control_regs::flush_global();
    }
}

/// Flush this CPU's translation of the user page `va` in the space tagged
/// `tag`, which this CPU runs.
pub fn flush_user_page(tag: u16, va: u64) {
    match pcid_active() {
        Some(have) => crate::arch::cpu::invpcid(have, Invpcid::Address, tag, va),
        None => crate::arch::cpu::invlpg(va),
    }
}

/// Flush every TLB entry on this CPU but the global ones, all PCIDs: what a
/// user-half change owes, since no user leaf is global.
pub fn flush_tlb_user() {
    if let Some(have) = pcid_active() {
        crate::arch::cpu::invpcid(have, Invpcid::AllButGlobal, 0, 0);
    } else {
        // SAFETY: the value is exactly what `read_cr3` just read on this CPU,
        // so writing it back is a valid CR3 write that only flushes the TLB.
        unsafe { crate::arch::cpu::write_cr3(crate::arch::cpu::read_cr3()) };
    }
}

/// CR3 register value: PML4 physical address | PCID.
#[derive(Clone, Copy)]
pub struct Cr3(u64);

/// The address space a CPU runs in, as the architecture names its root: CR3.
pub use Cr3 as Root;

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
    /// before this space took it.
    /// # Safety
    /// The underlying page tables must be valid and live.
    pub unsafe fn activate(self) {
        crate::arch::tlb::switching(self.pcid(), || {
            if pcid_active().is_some() {
                crate::arch::cpu::write_cr3(self.0 | CR3_NOFLUSH);
            } else {
                crate::arch::cpu::write_cr3(self.0);
            }
        });
    }

    /// Load CR3 with a flush; used during boot before PCID is enabled.
    /// # Safety
    /// The underlying page tables must be valid and live.
    pub unsafe fn load_flush(self) {
        crate::arch::cpu::write_cr3(self.0);
    }
}

/// The user PCID allocator: `kernel::pcid` owns the decision that no live tag is
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
            Self::Kernel => kernel::pcid::KERNEL_PCID,
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
                crate::arch::tlb::shootdown(crate::invalidation::Origin::Pcid, crate::invalidation::Half::User);
                pool.reclaim();
            }
            Alloc::Exhausted => return None,
        }
    }
}

/// Give back a table of `job`'s user space [`PageTablePage::try_new`] made,
/// which nothing links any more.
fn free_table(job: &Job, phys: u64) {
    // SAFETY: the caller unlinked the table, after the shootdown that let
    // every CPU stop walking it.
    unsafe { crate::mm::pmm::free_held(job, [phys]) };
}

/// The kernel half every space shares: PML4[256..511] and what they name.
/// Only boot fills its root's slots ([`seal_kernel_half`]); a user space
/// copies them when it is made.
struct KernelTables {
    root: u64,
}

impl KernelTables {
    fn root(&self) -> Cr3 {
        Cr3(self.root | kernel::pcid::KERNEL_PCID as u64)
    }

    fn root_table(&self) -> &PageTablePage {
        // SAFETY: the kernel's root is a table `init` made, which lives forever.
        unsafe { PageTablePage::from_phys(self.root) }
    }

    fn root_table_mut(&mut self) -> &mut PageTablePage {
        // SAFETY: as `root_table`; `&mut self` gives the exclusivity.
        unsafe { PageTablePage::from_phys_mut(self.root) }
    }

    /// Private: not safe to use until every CPU is told — the free fn [`map_mmio`] is the whole operation.
    fn map_mmio(&mut self, phys: u64, size: u64, cache: CachePolicy) -> crate::mm::Mmio {
        let start = phys & !(PAGE_2M - 1);
        let end = (phys + size + PAGE_2M - 1) & !(PAGE_2M - 1);
        let mut cur = start;
        while cur < end {
            self.map_2m(cur, kernel_leaf(cache, Prot::ReadWrite));
            cur += PAGE_2M;
        }
        crate::mm::Mmio::new(crate::mm::DirectMap::from_phys(phys), size)
    }

    /// Read from the table rather than remembered, or `None` if unmapped.
    fn policy_at(&self, virt: u64) -> Option<CachePolicy> {
        let (pml4_idx, pdpt_idx, pd_idx) = indices(virt);
        let pde = self.root_table().child(pml4_idx)?.child(pdpt_idx)?[pd_idx];
        if pde & PAGE_PRESENT == 0 {
            return None;
        }
        if pde & PAGE_SIZE_BIT == 0 {
            let pte = self.root_table().child(pml4_idx)?.child(pdpt_idx)?.child(pd_idx)?
                [((virt >> 12) & 0x1FF) as usize];
            return (pte & PAGE_PRESENT != 0).then(|| CachePolicy::from_pte(pte));
        }
        Some(CachePolicy::from_pde(pde))
    }

    pub fn direct_map_policy(&self, phys: u64) -> Option<CachePolicy> {
        self.policy_at(crate::mm::DirectMap::from_phys(phys).as_ptr::<u8>() as u64)
    }

    /// Permanent: the caller must own `phys` for the machine's life, since
    /// handing the enclosing 2 MiB page back to the PMM would reissue memory with a hole.
    pub fn guard_4k(&mut self, phys: u64) {
        assert!(phys & 0xFFF == 0, "guard_4k: phys {phys:#x} not 4 KiB-aligned");
        let virt = crate::mm::DirectMap::from_phys(phys).as_ptr::<u8>() as u64;
        let (pml4_idx, pdpt_idx, pd_idx) = indices(virt);
        let pd_phys = {
            let pdpt = self.root_table().child(pml4_idx).expect("guard_4k: no PDPT over the direct map");
            let entry = pdpt[pdpt_idx];
            assert!(entry & PAGE_PRESENT != 0, "guard_4k: no PD over the direct map");
            entry & ADDR_MASK
        };
        // SAFETY: `pd_phys` names the direct map's PD, living forever;
        // `&mut self` here is `KERNEL.lock()`'s required exclusivity.
        let pd = unsafe { PageTablePage::from_phys_mut(pd_phys) };
        let pde = pd[pd_idx];
        assert!(pde & PAGE_PRESENT != 0, "guard_4k: {phys:#x} is not in the direct map");
        // Both writes discharge on this CPU alone: the guard belongs to one
        // CPU, so only it dereferences the removed page; `alloc_idle_stack`
        // runs on the BSP before an AP's TLB exists, and a sibling's stale
        // 2 MiB entry stays correct for the other 511 pages it still maps.
        let target = self.root();

        // Already split: an earlier guard in the same 2 MiB region.
        if pde & PAGE_SIZE_BIT != 0 {
            let base = pde & ADDR_MASK_2M;
            let flags = pde & !ADDR_MASK_2M & !PAGE_SIZE_BIT;
            assert!(
                flags & PAGE_PAT_2M == 0,
                "guard_4k: {phys:#x} carries a PAT bit that is an address bit in a 4 KiB PTE"
            );
            let pt = PageTablePage::new();
            for i in 0..512 {
                pt.init_entry(i, (base + i as u64 * 4096) | flags);
            }
            let pt_phys = pt.phys();
            pd.write_pde(pd_idx, virt, pt_phys | KERNEL_TABLE).discharge(target);
        }

        // SAFETY: `pd[pd_idx]` names a page table either way, permanently —
        // same lock exclusivity as above.
        let pt = unsafe { PageTablePage::from_phys_mut(pd[pd_idx] & ADDR_MASK) };
        let idx = ((phys >> 12) & 0x1FF) as usize;
        assert!(pt[idx] & PAGE_PRESENT != 0, "guard_4k: {phys:#x} is already unmapped");
        pt.write(idx, virt, 0).discharge(target);
    }

    /// Replaces whatever is there; a page `guard_4k` already split must not
    /// reach here.
    fn map_2m(&mut self, phys: u64, flags: u64) {
        let virt = crate::mm::DirectMap::from_phys(phys).as_ptr::<u8>() as u64;
        let pd_idx = indices(virt).2;
        let pd = self.ensure_table(virt, KERNEL_TABLE);
        let entry = phys | flags | PAGE_SIZE_BIT;
        let existing = pd[pd_idx];
        assert!(
            existing & PAGE_PRESENT == 0
                || existing & !(PAGE_ACCESSED | PAGE_DIRTY) == entry
                || (existing & PAGE_SIZE_BIT != 0
                    && CachePolicy::from_pde(existing) == CachePolicy::Normal),
            "map_2m: {phys:#x} is mapped {existing:#x} and cannot also be {entry:#x}"
        );
        // Neither caller wants the single address: [`map_mmio`] flushes every
        // CPU (type may have changed under a sibling); `init` has no TLB yet.
        pd.write_pde(pd_idx, virt, entry).subsumed_by_flush();
    }

    /// The 2 MiB page of the direct map at `phys`, which holds more than one of
    /// the image's protections, in 4 KiB leaves each as the image says. Only
    /// `init` calls it, before any TLB holds these tables.
    fn map_image_pages(&mut self, phys: u64, image: &crate::mm::Image) {
        let virt = crate::mm::DirectMap::from_phys(phys).as_ptr::<u8>() as u64;
        let pt = PageTablePage::new();
        for i in 0..512 {
            let page = phys + i as u64 * PAGE_4K;
            pt.init_entry(i, page | kernel_leaf(CachePolicy::Normal, image.prot(page).unwrap_or(Prot::ReadWrite)));
        }
        let pt_phys = pt.phys();
        let pd = self.ensure_table(virt, KERNEL_TABLE);
        pd.write_pde(indices(virt).2, virt, pt_phys | KERNEL_TABLE).expect_install("map_image_pages");
    }

    /// Masks `flags` to `TABLE_FLAGS`: bit 12 is a leaf's PAT bit but an
    /// upper entry's address bit, so leaf flags can't move a table 4 KiB.
    fn ensure_table(&mut self, va: u64, flags: u64) -> &mut PageTablePage {
        let flags = flags & TABLE_FLAGS;
        let (pml4_idx, pdpt_idx, _) = indices(va);
        let target = self.root();
        let root = self.root_table_mut();

        if root[pml4_idx] & PAGE_PRESENT == 0 {
            let child = PageTablePage::new();
            root.write(pml4_idx, va, child.phys() | flags).expect_install("ensure_table: pml4");
        } else {
            // Upper-level entries: widen only (OR), never narrow.
            root.widen(pml4_idx, va, flags).discharge(target);
        }

        // SAFETY: the branch above guaranteed PRESENT, so `& ADDR_MASK`
        // names a table this space owns for life; `&mut self` gives the
        // exclusivity `from_phys_mut` requires.
        let pdpt = unsafe { PageTablePage::from_phys_mut(root[pml4_idx] & ADDR_MASK) };

        if pdpt[pdpt_idx] & PAGE_PRESENT == 0 {
            let child = PageTablePage::new();
            pdpt.write(pdpt_idx, va, child.phys() | flags).expect_install("ensure_table: pdpt");
        } else {
            pdpt.widen(pdpt_idx, va, flags).discharge(target);
        }

        // SAFETY: same argument as `pdpt` above, one level down.
        unsafe { PageTablePage::from_phys_mut(pdpt[pdpt_idx] & ADDR_MASK) }
    }

    /// An empty second-level table under the kernel's empty root slot `slot`.
    fn install_root(&mut self, slot: usize) {
        let child = PageTablePage::new();
        let va = crate::mm::PHYS_OFFSET + ((slot - ROOT_HIGH_HALF) as u64) * (1 << 39);
        self.root_table_mut()
            .write(slot, va, child.phys() | PAGE_PRESENT | PAGE_WRITE)
            .expect_install("install_root");
    }
}

/// The kernel half's tables; its root is 0 until `init` builds it.
static KERNEL: Lock<KernelTables> = Lock::new(KernelTables { root: 0 });

/// The direct map's memory type for `phys`, read off the kernel's tables.
pub fn direct_map_policy(phys: u64) -> Option<CachePolicy> {
    KERNEL.lock().direct_map_policy(phys)
}

/// Take the 4 KiB page at `phys` out of the direct map for good: the caller
/// owns it for the machine's life.
pub fn guard_4k(phys: u64) {
    KERNEL.lock().guard_4k(phys);
}

/// The kernel wrote instructions into the frame at `phys` that a user leaf
/// executes: nothing to do, as the instruction and data caches are coherent
/// (SDM Vol. 3A §11.6).
pub fn wrote_code(_phys: u64) {}

/// Marks a user leaf whose frame the space owns: one of the bits a PTE leaves
/// to software (SDM Vol. 3A §4.5, Table 4-19). Kept on a leaf that is not
/// present, so a frame no access may reach is still recorded.
const OWNED: u64 = 1 << 9;

/// What one user page's leaf says.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Leaf {
    Empty,
    Mapped(Mapping),
    /// A frame the space owns that no access reaches.
    Held(u64),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Mapping {
    pub phys: u64,
    pub prot: Prot,
    /// The frame is the space's own, freed with the leaf; otherwise an
    /// object's or a device's.
    pub owned: bool,
    pub cache: CachePolicy,
}

impl Leaf {
    fn encode(self) -> u64 {
        match self {
            Leaf::Empty => 0,
            Leaf::Held(phys) => phys | OWNED,
            Leaf::Mapped(m) => {
                m.phys | m.prot.leaf_bits() | m.cache.pte_bits() | if m.owned { OWNED } else { 0 }
            }
        }
    }

    fn decode(pte: u64) -> Self {
        if pte & PAGE_PRESENT == 0 {
            return if pte & OWNED != 0 { Leaf::Held(pte & ADDR_MASK) } else { Leaf::Empty };
        }
        let prot = match (pte & PAGE_WRITE != 0, pte & PAGE_NX != 0) {
            (true, _) => Prot::ReadWrite,
            (false, true) => Prot::Read,
            (false, false) => Prot::ReadExec,
        };
        Leaf::Mapped(Mapping { phys: pte & ADDR_MASK, prot, owned: pte & OWNED != 0, cache: CachePolicy::from_pte(pte) })
    }
}

/// The indices of `[start, end)`'s entries in the table of entries `1 <<
/// shift` bytes apart that covers `[base, base + 512 << shift)`.
fn slots(start: u64, end: u64, base: u64, shift: u32) -> core::ops::Range<usize> {
    let (lo, hi) = (start.max(base), end.min(base + (512 << shift)));
    if lo >= hi {
        return 0..0;
    }
    (((lo - base) >> shift) as usize)..(((hi - 1 - base) >> shift) as usize + 1)
}

/// The leaves of one 2 MiB of a user space, [`UserTables::leaf_table`]'s.
pub struct LeafTable<'a> {
    base: u64,
    table: &'a mut PageTablePage,
}

impl LeafTable<'_> {
    /// Whether `va` is one of these leaves' pages.
    pub fn covers(&self, va: u64) -> bool {
        va & !(PAGE_2M - 1) == self.base
    }

    fn slot(&self, va: u64) -> usize {
        assert!(self.covers(va), "paging: {va:#x} outside the leaves from {:#x}", self.base);
        ((va - self.base) >> 12) as usize
    }

    pub fn get(&self, va: u64) -> Leaf {
        Leaf::decode(self.table.0[self.slot(va)])
    }

    pub fn set(&mut self, va: u64, leaf: Leaf) {
        let slot = self.slot(va);
        self.table.store(slot, leaf.encode());
    }

    /// Write `fill`'s leaf into each empty leaf of `[start, end)`, all of
    /// whose pages these leaves cover.
    pub fn fill(&mut self, start: u64, end: u64, mut fill: impl FnMut(u64) -> Leaf) {
        assert!(start < end && self.covers(start) && self.covers(end - 1), "paging: a fill of {start:#x}..{end:#x}");
        for i in slots(start, end, self.base, 12) {
            if Leaf::decode(self.table.0[i]) == Leaf::Empty {
                self.table.store(i, fill(self.base + ((i as u64) << 12)).encode());
            }
        }
    }
}

/// Tables [`UserTables::unlink`] took out of the tree, freed once no CPU can
/// still walk into them. Each holds no leaf, so they are chained through
/// their first entry, which holds the next one's frame: a value with its low
/// bit clear, which a walker that still reaches one reads as no entry.
pub struct Unlinked {
    head: u64,
}

impl Unlinked {
    /// No table: what a change that keeps every table owes.
    pub fn none() -> Self {
        Self { head: 0 }
    }

    pub fn is_empty(&self) -> bool {
        self.head == 0
    }

    fn push(&mut self, phys: u64) {
        // SAFETY: a table `unlink` just took out of the tree, which nothing
        // else frees, holding no entry.
        let table = unsafe { PageTablePage::from_phys_mut(phys) };
        assert!(table.0.iter().all(|&e| e == 0), "paging: a table unlinked at {phys:#x} still holds an entry");
        table.store(0, self.head);
        self.head = phys;
    }

    /// Free the tables of `job`'s space, only after the shootdown that
    /// followed the unlink.
    fn free(self, job: &Job) {
        let mut at = self.head;
        while at != 0 {
            // SAFETY: a table of the chain, which only this frees.
            let next = unsafe { PageTablePage::from_phys_mut(at) }.0[0];
            free_table(job, at);
            at = next;
        }
    }
}

/// One user space's tables: PML4[0..255] its own, PML4[256..511] the
/// kernel's, copied in when it is made. `mm::space` decides what every leaf
/// holds; this only reads and writes them. A write here owes no invalidation
/// when the entry was empty, and the caller's [`shoot_down`](Self::shoot_down)
/// otherwise.
pub struct UserTables {
    root: u64,
    /// Owned for this space's life: dropping it returns a user tag, so two live
    /// spaces can never share one.
    pcid: PcidHandle,
    /// The job its tables are counted to.
    job: Arc<Job>,
}

impl UserTables {
    /// A new space with the kernel half copied in, or `None` when every user
    /// PCID is held by a live space or no frame is free for its root. Its
    /// tables are counted to `job`.
    pub fn new(job: &Arc<Job>) -> Option<Self> {
        let pml4 = PageTablePage::try_new(job)?;
        let Some(pcid) = alloc_pcid() else {
            free_table(job, pml4.phys());
            return None;
        };
        let kernel = KERNEL.lock();
        for slot in ROOT_HIGH_HALF..512 {
            let entry = kernel.root_table()[slot];
            assert!(
                entry & PAGE_PRESENT != 0,
                "new_user: kernel root slot {slot} is absent, and this space would never see what is \
                 mapped there: `seal_kernel_half` runs before the first user space"
            );
            pml4.init_entry(slot, entry);
        }
        Some(Self { root: pml4.phys(), pcid: PcidHandle::User(pcid), job: Arc::clone(job) })
    }

    /// The kernel's own space, whose user half is empty: what a kernel thread
    /// and an idle CPU run under. Never dropped.
    pub fn kernel() -> Self {
        let root = KERNEL.lock().root;
        assert!(root != 0, "paging: the kernel's space before `init`");
        Self { root, pcid: PcidHandle::Kernel, job: Arc::clone(crate::mm::charge::boot()) }
    }

    pub fn root(&self) -> Cr3 {
        Cr3(self.root | self.pcid.value() as u64)
    }

    fn root_table(&self) -> &PageTablePage {
        // SAFETY: the root is a table this space made and owns for its life.
        unsafe { PageTablePage::from_phys(self.root) }
    }

    fn root_table_mut(&mut self) -> &mut PageTablePage {
        // SAFETY: as `root_table`; `&mut self` gives the exclusivity.
        unsafe { PageTablePage::from_phys_mut(self.root) }
    }

    /// The table of leaves over `va`, if one is built.
    fn leaves(&self, va: u64) -> Option<&PageTablePage> {
        let (a, b, c) = indices(va);
        let pd = self.root_table().child(a)?.child(b)?;
        (pd[c] & PAGE_SIZE_BIT == 0).then(|| pd.child(c)).flatten()
    }

    /// Build every table `[start, end)`'s leaves need; answers how many
    /// frames that took, or `None` where a frame ran out, leaving what it
    /// built linked for an [`unlink`](Self::unlink). Only an empty entry is
    /// written, which owes nothing.
    pub fn build(&mut self, start: u64, end: u64) -> Option<u64> {
        let job = Arc::clone(&self.job);
        let mut built = 0;
        let mut va = start & !(PAGE_2M - 1);
        while va < end {
            let (a, b, c) = indices(va);
            let mut level = self.root_table_mut();
            for index in [a, b, c] {
                if level[index] & PAGE_PRESENT == 0 {
                    let child = PageTablePage::try_new(&job)?;
                    level.write(index, va, child.phys() | TABLE_FLAGS).expect_install("build");
                    built += 1;
                }
                // SAFETY: present, so a table this space linked; `&mut self`.
                level = unsafe { PageTablePage::from_phys_mut(level[index] & ADDR_MASK) };
            }
            va += PAGE_2M;
        }
        Some(built)
    }

    /// The leaf over `va`, empty where no table holds one.
    pub fn get(&self, va: u64) -> Leaf {
        self.leaves(va).map_or(Leaf::Empty, |t| Leaf::decode(t[((va >> 12) & 0x1FF) as usize]))
    }

    /// Write the leaf over `va`, whose table [`build`](Self::build) made.
    pub fn set(&mut self, va: u64, leaf: Leaf) {
        self.leaf_table(va).unwrap_or_else(|| panic!("paging: no table over {va:#x}")).set(va, leaf);
    }

    /// The table of leaves over `va`'s 2 MiB, if [`build`](Self::build) made
    /// one: every leaf of it read and written for one walk.
    pub fn leaf_table(&mut self, va: u64) -> Option<LeafTable<'_>> {
        assert!(toyos_userbound::is_user_addr(va), "paging: a user leaf at {va:#x}");
        let table = self.leaves(va).map(PageTablePage::phys)?;
        // SAFETY: a table of leaves this space linked; `&mut self`.
        Some(LeafTable { base: va & !(PAGE_2M - 1), table: unsafe { PageTablePage::from_phys_mut(table) } })
    }

    /// Call `f` on every leaf in `[start, end)` that is not empty, with its
    /// address; a `Some` it answers is written in its place.
    pub fn edit(&mut self, start: u64, end: u64, mut f: impl FnMut(u64, Leaf) -> Option<Leaf>) {
        self.each_table(start, end, |base, table| {
            for i in slots(start, end, base, 12) {
                let leaf = Leaf::decode(table.0[i]);
                if leaf != Leaf::Empty {
                    if let Some(new) = f(base + ((i as u64) << 12), leaf) {
                        table.store(i, new.encode());
                    }
                }
            }
        });
    }

    /// [`take_owned`](Self::take_owned), where `decide` says of each frame of
    /// a table's batch, with the address its leaf maps, whether the leaf
    /// keeps it: a kept frame's leaf stays, every other is emptied.
    pub fn discard_owned(&mut self, start: u64, end: u64, mut decide: impl FnMut(&[u64], &[u64], &mut [bool])) {
        self.each_table(start, end, |base, table| {
            let mut frames = [0u64; 512];
            let mut vas = [0u64; 512];
            let mut slot = [0usize; 512];
            let mut n = 0;
            for i in slots(start, end, base, 12) {
                if let Leaf::Held(phys) | Leaf::Mapped(Mapping { phys, owned: true, .. }) = Leaf::decode(table.0[i]) {
                    frames[n] = phys;
                    vas[n] = base + ((i as u64) << 12);
                    slot[n] = i;
                    n += 1;
                }
            }
            if n == 0 {
                return;
            }
            let mut kept = [false; 512];
            decide(&frames[..n], &vas[..n], &mut kept[..n]);
            for k in 0..n {
                if !kept[k] {
                    table.store(slot[k], Leaf::Empty.encode());
                }
            }
        });
    }

    /// Empty every leaf of `[start, end)` holding a frame the space owns, one
    /// table of leaves at a time, handing each table's frames to `taken`: at
    /// most 512 at once, in a buffer of its own, so nothing is allocated.
    pub fn take_owned(&mut self, start: u64, end: u64, mut taken: impl FnMut(&mut [u64])) {
        self.each_table(start, end, |base, table| {
            let mut frames = [0u64; 512];
            let mut n = 0;
            for i in slots(start, end, base, 12) {
                if let Leaf::Held(phys) | Leaf::Mapped(Mapping { phys, owned: true, .. }) = Leaf::decode(table.0[i]) {
                    table.store(i, Leaf::Empty.encode());
                    frames[n] = phys;
                    n += 1;
                }
            }
            if n > 0 {
                taken(&mut frames[..n]);
            }
        });
    }

    /// Call `f` on every table of leaves meeting `[start, end)`, with the
    /// address its first leaf maps.
    fn each_table(&mut self, start: u64, end: u64, mut f: impl FnMut(u64, &mut PageTablePage)) {
        let root = self.root_table_mut();
        for a in slots(start, end, 0, 39).filter(|&a| a < ROOT_HIGH_HALF) {
            let base_a = (a as u64) << 39;
            let Some(pdpt) = root.child_mut(a) else { continue };
            for b in slots(start, end, base_a, 30) {
                let base_b = base_a + ((b as u64) << 30);
                let Some(pd) = pdpt.child_mut(b) else { continue };
                for c in slots(start, end, base_b, 21) {
                    if pd[c] & PAGE_SIZE_BIT != 0 {
                        continue;
                    }
                    if let Some(pt) = pd.child_mut(c) {
                        f(base_b + ((c as u64) << 21), pt);
                    }
                }
            }
        }
    }

    /// Unlink every table over `[start, end)` whose span `needed` answers
    /// `false` for, and every table above one that is then empty; a table
    /// that still links anything stays. A table unlinked holds no leaf: a span
    /// no range needs maps nothing. They are freed by [`free`](Self::free),
    /// after the caller's shootdown.
    /// Free tables [`unlink`](Self::unlink) took out, only after the
    /// shootdown that followed it.
    pub fn free(&self, unlinked: Unlinked) {
        unlinked.free(&self.job);
    }

    pub fn unlink(&mut self, start: u64, end: u64, needed: impl Fn(u64, u64) -> bool) -> Unlinked {
        let mut unlinked = Unlinked::none();
        let root = self.root_table_mut();
        for a in slots(start, end, 0, 39).filter(|&a| a < ROOT_HIGH_HALF) {
            let base_a = (a as u64) << 39;
            let Some(pdpt) = root.child_mut(a) else { continue };
            for b in slots(start, end, base_a, 30) {
                let base_b = base_a + ((b as u64) << 30);
                let Some(pd) = pdpt.child_mut(b) else { continue };
                for c in slots(start, end, base_b, 21) {
                    let span = base_b + ((c as u64) << 21);
                    if pd[c] & PAGE_PRESENT != 0 && !needed(span, span + PAGE_2M) {
                        let table = pd[c] & ADDR_MASK;
                        pd.store(c, 0);
                        unlinked.push(table);
                    }
                }
                if pd.0.iter().all(|&e| e == 0) && !needed(base_b, base_b + (1 << 30)) {
                    let table = pdpt[b] & ADDR_MASK;
                    pdpt.store(b, 0);
                    unlinked.push(table);
                }
            }
            if pdpt.0.iter().all(|&e| e == 0) && !needed(base_a, base_a + (1 << 39)) {
                let table = root[a] & ADDR_MASK;
                root.store(a, 0);
                unlinked.push(table);
            }
        }
        unlinked
    }

    /// Every CPU has dropped what it cached of `[start, end)` and of the
    /// tables `unlinked` took out before this returns: every CPU's user
    /// translations go, paging-structure caches with them.
    /// Answers whether it reached another CPU.
    pub fn shoot_down(&self, start: u64, end: u64, unlinked: &Unlinked) -> bool {
        // A table taken away is cached apart from the leaves: by page is not
        // enough for it.
        let (start, end) = if unlinked.is_empty() { (start, end) } else { (0, u64::MAX) };
        crate::arch::tlb::user_change(crate::invalidation::Origin::Unmap, self.pcid.value(), start, end)
    }
}

/// Frees every table under the user half and the root; the frames its leaves
/// map were freed by their owners first. The kernel half is shared, and the
/// kernel's own space never drops. No CPU walks a space that drops: its last
/// thread has left it, and its tag is flushed everywhere before it is issued
/// again.
impl Drop for UserTables {
    fn drop(&mut self) {
        assert!(matches!(self.pcid, PcidHandle::User(_)), "paging: the kernel's address space dropped");
        let all = self.unlink(0, (ROOT_HIGH_HALF as u64) << 39, |_, _| false);
        all.free(&self.job);
        free_table(&self.job, self.root);
    }
}

/// Kernel CR3, cached for lock-free access from panic/crash paths.
static KERNEL_CR3: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Kernel CR3. Lock-free — safe to call from panic context.
pub fn kernel_root() -> Cr3 {
    Cr3(KERNEL_CR3.load(core::sync::atomic::Ordering::Relaxed))
}

/// Leave whatever user address space is current for the kernel's own. Safe,
/// unlike [`Cr3::activate`]: the kernel tables never move, so there's no
/// argument left for a caller to get wrong. Outgoing user mappings stop
/// being addressable at this line.
pub fn activate_kernel() {
    // SAFETY: `KERNEL_CR3` names the boot-built tables, mapping the code and
    // stack this call returns onto.
    unsafe { kernel_root().activate() };
}

/// [`activate_kernel`] for a CPU without `CR4.PCIDE` yet: `activate` sets the
/// reserved `NOFLUSH` bit once the *machine* declares PCIDE, which an AP
/// reaches before setting it on itself.
pub fn load_kernel_flush() {
    // SAFETY: same as `activate_kernel` above.
    unsafe { kernel_root().load_flush() };
}

/// Every window [`map_mmio`] mapped, as `(start, end)`: what this kernel
/// drives a device through, and so what it refuses the `acpi` claim's holder
/// (`arch::acpi_mode`), who is refused every page one lies in. Its only writer
/// is the only maker of such a window. One entry a distinct window and none
/// removed: a boot's drivers map theirs once, and past boot only `pcidev`
/// maps, a claimed function's MSI-X entry where firmware put its table.
static DRIVEN: Lock<Vec<(u64, u64)>> = Lock::new(Vec::new());

/// Run `f` over every window this kernel mapped to drive a device.
pub fn with_driven_windows<T>(f: impl FnOnce(&[(u64, u64)]) -> T) -> T {
    f(&DRIVEN.lock())
}

/// What [`map_mmio`] maps at: whole 2 MiB pages, so a window off this grain
/// takes its neighbours' bytes with it.
pub const MMIO_GRAIN: u64 = PAGE_2M;

/// Free function (not a method): the lock and the shootdown are separate
/// statements. Not optional — `map_2m` may change memory type under a
/// sibling's stale entry, which is SDM Vol. 3A §11.12.4 undefined behaviour.
pub fn map_mmio(phys: u64, size: u64, policy: MmioPolicy) -> crate::mm::Mmio {
    let mmio = KERNEL.lock().map_mmio(phys, size, policy.cache());
    {
        let mut driven = DRIVEN.lock();
        if !driven.contains(&(phys, phys + size)) {
            driven.push((phys, phys + size));
        }
    }
    crate::arch::tlb::shootdown(crate::invalidation::Origin::Mmio, crate::invalidation::Half::Kernel);
    // Read back off the table and logged beside firmware's MTRR verdict: the
    // boot's own evidence that no register window trusts firmware.
    let installed =
        KERNEL.lock().direct_map_policy(phys).expect("map_mmio: the window was just mapped");
    assert!(installed == policy.cache(), "map_mmio: {phys:#x} installed {installed:?}");
    crate::log!(
        "mmio: {phys:#x}+{size:#x} PAT {installed:?} (MTRR {})",
        crate::arch::mtrr::range_type(phys, size).name()
    );
    mmio
}

/// Where [`debug_global_leaf`] maps: the kernel half's last 2 MiB, under a root
/// slot [`seal_kernel_half`] installed, as far past the direct map as it goes.
#[cfg(feature = "test-actuators")]
const SCRATCH: u64 = 0u64.wrapping_sub(PAGE_2M);

/// `SYS_DEBUG`'s `GLOBAL_LEAF_REWRITE`: a [`kernel_leaf`] at [`SCRATCH`] over one
/// frame, read so this CPU caches it global; the leaf pointed at a second frame
/// and read again with nothing invalidated, the control; invalidated as `how`
/// asks; read a third time. Answers which frame the third read saw, or refuses:
/// where the control already saw the second frame, or where a user-half
/// shootdown's answer here reached wide because another CPU's kernel-half
/// generation landed inside it. The kernel's lock keeps it on one CPU and
/// interrupts stay closed, so no flush but `how`'s lands between the reads.
#[cfg(feature = "test-actuators")]
pub fn debug_global_leaf(how: u64) -> u64 {
    use crate::invalidation::{Half, Origin};
    use toyos_abi::syscall::debug_action as da;
    use toyos_abi::syscall::SyscallError;

    enum How {
        Discharged,
        ShotDown(Half),
    }
    let how = match how {
        da::GLOBAL_LEAF_DISCHARGED => How::Discharged,
        da::GLOBAL_LEAF_KERNEL_SHOT_DOWN => How::ShotDown(Half::Kernel),
        da::GLOBAL_LEAF_USER_SHOT_DOWN => How::ShotDown(Half::User),
        _ => return SyscallError::InvalidArgument.to_u64(),
    };
    let (Some(old), Some(new)) = (crate::mm::pmm::alloc_page(), crate::mm::pmm::alloc_page()) else {
        return SyscallError::ResourceExhausted.to_u64();
    };
    // Each frame's first word is its own address, so a read names its frame.
    for page in [&old, &new] {
        let at = page.direct_map();
        // SAFETY: a frame this call owns, through the direct map.
        unsafe { core::ptr::write_volatile(at.as_mut_ptr::<u64>(), at.phys()) };
    }
    let (old_phys, new_phys) = (old.direct_map().phys(), new.direct_map().phys());
    let leaf = kernel_leaf(CachePolicy::Normal, Prot::ReadWrite) | PAGE_SIZE_BIT;
    // SAFETY: every read below is made while `SCRATCH` maps one of the two
    // frames this call owns, or a translation of one of them.
    let read = || unsafe { core::ptr::read_volatile(SCRATCH as *const u64) };

    let (held, widened, seen) = {
        let mut space = KERNEL.lock();
        let _closed = crate::arch::IrqGuard::close();
        let target = space.root();
        let pd = space.ensure_table(SCRATCH, KERNEL_TABLE);
        let idx = indices(SCRATCH).2;
        if pd[idx] & PAGE_PRESENT != 0 {
            return SyscallError::AlreadyExists.to_u64();
        }
        pd.write_pde(idx, SCRATCH, old_phys | leaf).expect_install("debug_global_leaf");
        let first = read();
        assert!(first == old_phys, "debug_global_leaf: a fresh leaf over {old_phys:#x} read {first:#x}");
        let owed = pd.write_pde(idx, SCRATCH, new_phys | leaf);
        let held = read();
        let widened = match how {
            How::Discharged => {
                owed.discharge(target);
                false
            }
            // The write's own invalidation is withheld: the arm asks what the shootdown drops.
            How::ShotDown(half) => {
                let _ = owed;
                crate::arch::tlb::debug_shootdown_answered_wide(Origin::Staged, half) && matches!(half, Half::User)
            }
        };
        let seen = read();
        pd.write_pde(idx, SCRATCH, 0).discharge(target);
        (held, widened, seen)
    };
    // No CPU keeps a translation of either frame past its free.
    crate::arch::tlb::shootdown(Origin::Staged, Half::Kernel);
    drop((old, new));
    let old_frame = |at: u64| match at {
        _ if at == old_phys => true,
        _ if at == new_phys => false,
        _ => panic!("debug_global_leaf: the leaf over {old_phys:#x} then {new_phys:#x} read {at:#x}"),
    };
    match (old_frame(held), widened, old_frame(seen)) {
        (false, _, _) => da::GLOBAL_LEAF_UNHELD,
        (true, true, _) => da::GLOBAL_LEAF_WIDENED,
        (true, false, true) => da::GLOBAL_LEAF_SAW_OLD,
        (true, false, false) => da::GLOBAL_LEAF_SAW_NEW,
    }
}

/// Build kernel page tables: the direct map in the high half, in 2 MiB pages
/// but where one holds more than one of `image`'s protections, as far as
/// [`toyos_bootmap::x86_64::direct_map_end`] reaches, and answer that end. The
/// scanout is not mapped for itself: in the low map the MTRRs type it until
/// `panic_console::remap` gives it a type of its own.
pub(crate) fn init(
    memory_map: &[MemoryMapEntry],
    _scanout: Option<(u64, u64)>,
    image: &crate::mm::Image,
) -> toyos_bootmap::DirectMapEnd {
    let extent = toyos_bootmap::x86_64::direct_map_end(memory_map)
        .unwrap_or_else(|refusal| panic!("paging: firmware's memory map: {refusal}"));
    let end = extent.get();

    let mut kernel = KERNEL.lock();
    kernel.root = PageTablePage::new().phys();

    // Only the direct map's slots: the rest wait for `seal_kernel_half`.
    let last_slot = indices(crate::mm::DirectMap::from_phys(end - 1).as_ptr::<u8>() as u64).0;
    for slot in ROOT_HIGH_HALF..=last_slot {
        kernel.install_root(slot);
    }
    let mut addr: u64 = 0;
    while addr < end {
        match image.block(addr) {
            Some(prot) => kernel.map_2m(addr, kernel_leaf(CachePolicy::Normal, prot)),
            None => kernel.map_image_pages(addr, image),
        }
        addr += PAGE_2M;
    }
    crate::log!("paging: the direct map covers 0x0..{end:#x}");

    let cr3 = kernel.root();
    drop(kernel);
    KERNEL_CR3.store(cr3.0, core::sync::atomic::Ordering::Release);
    // SAFETY: `cr3` names the space this function just finished building via
    // the `map_2m` loop above; nothing has run under it yet, so live reduces
    // to self-consistent.
    unsafe {
        cr3.load_flush();
    }
    extent
}

/// Install a second-level table under every kernel root slot [`init`] left
/// empty, from the pmm's heap, before the first user space copies the slots:
/// one installed after a space was made would be missing from that space.
pub(crate) fn seal_kernel_half() {
    let mut kernel = KERNEL.lock();
    for slot in ROOT_HIGH_HALF..512 {
        if kernel.root_table()[slot] & PAGE_PRESENT == 0 {
            kernel.install_root(slot);
        }
    }
}

/// [`kernel::wx::audit`] of the kernel's tables, whose kernel half every
/// address space shares.
pub fn audit_wx(text: core::ops::Range<u64>) -> kernel::wx::Verdict {
    let space = KERNEL.lock();
    // SAFETY: every table the kernel's root names is one this module linked,
    // and the lock held keeps them in place while they are read.
    let table = |phys: u64| unsafe { &PageTablePage::from_phys(phys).0 };
    kernel::wx::audit::<kernel::wx::X86_64>(&space.root_table().0, text, &table)
}

/// `kernel-wx-leaf`: the direct map's leaf over `phys` made executable, so
/// [`audit_wx`] has a writable executable leaf to find. Answers its address.
#[cfg(feature = "boot-actuators")]
pub fn stage_writable_executable(phys: u64) -> u64 {
    let virt = crate::mm::DirectMap::from_phys(phys).as_ptr::<u8>() as u64;
    let mut space = KERNEL.lock();
    let target = space.root();
    let pd = space.ensure_table(virt, KERNEL_TABLE);
    let i = indices(virt).2;
    let pde = pd[i];
    assert!(pde & PAGE_PRESENT != 0, "kernel-wx-leaf: {phys:#x} is not in the direct map");
    let (table, slot, at) = if pde & PAGE_SIZE_BIT != 0 {
        (pd, i, virt & !(PAGE_2M - 1))
    } else {
        // SAFETY: a present PDE without the size bit names a table this
        // module linked; the kernel's lock gives the exclusivity.
        (unsafe { PageTablePage::from_phys_mut(pde & ADDR_MASK) }, ((virt >> 12) & 0x1FF) as usize, virt & !(PAGE_4K - 1))
    };
    let leaf = table[slot];
    assert!(leaf & PAGE_PRESENT != 0 && leaf & PAGE_WRITE != 0, "kernel-wx-leaf: {phys:#x} is not under a writable leaf");
    table.write(slot, at, leaf & !PAGE_NX).discharge(target);
    at
}

fn has(entry: u64, flag: u64) -> u8 {
    if entry & flag != 0 {
        1
    } else {
        0
    }
}

/// The *currently loaded* CR3, not `kernel_root()` (a panic can run on a user
/// space); lock-free and silent, for the panic path to prove a mapping
/// before writing through it.
pub fn present_in_current_tables(addr: u64) -> bool {
    translate_in_current_tables(addr).is_some()
}

/// Where `addr` lands in the tables this CPU runs under now, whatever the
/// leaf's size; lock-free, for a crash path that may take no lock. Rights are
/// not asked: a caller reading on a process's behalf refuses a non-user
/// address itself.
pub fn translate_in_current_tables(addr: u64) -> Option<crate::mm::DirectMap> {
    // SAFETY: `Cr3::current().phys()` names the table this CPU runs under
    // right now, so it can't be freed meanwhile. No lock: each entry read is
    // one aligned `u64`, atomic at the hardware level, so no read is torn.
    let mut table = unsafe { PageTablePage::from_phys(Cr3::current().phys()) };
    for level in 0..3 {
        let shift = 39 - level * 9;
        let entry = table[((addr >> shift) & 0x1FF) as usize];
        if entry & PAGE_PRESENT == 0 {
            return None;
        }
        if level > 0 && entry & PAGE_SIZE_BIT != 0 {
            let span = 1u64 << shift;
            return Some(crate::mm::DirectMap::from_phys(
                (entry & ADDR_MASK & !(span - 1)) | (addr & (span - 1)),
            ));
        }
        // SAFETY: PRESENT just checked; same argument as above the loop — a
        // present entry under the current CR3 names a live table.
        table = unsafe { PageTablePage::from_phys(entry & ADDR_MASK) };
    }
    let pte = table[((addr >> 12) & 0x1FF) as usize];
    (pte & PAGE_PRESENT != 0)
        .then(|| crate::mm::DirectMap::from_phys((pte & ADDR_MASK) | (addr & 0xFFF)))
}

/// Give every 2 MiB leaf covering `[phys, phys + size)` the write-combining
/// type, in the page tables this CPU is running under — the loader's, before
/// this kernel has any of its own.
///
/// **The panel's first paint is what this is for**, and [`map_mmio`] cannot
/// answer before `mm::init` has built a table to map into while the panel arms
/// long before that.
///
/// Both windows the loader opened onto the scanout are switched, identity and
/// direct map, because one physical page may not hold two memory types
/// (SDM Vol. 3A §11.12.4) — and every leaf of both is proved before any one of
/// them is written, so a range that cannot be switched whole is not switched
/// at all rather than left holding one type in one window and another in the
/// other.
///
/// `false` is such a range — nothing mapped there, or not in 2 MiB leaves —
/// and the caller keeps whatever the loader left and says so.
/// Before `mm::init` only: after it, the kernel's own tables are the ones to
/// map into and [`map_mmio`] is the way.
pub fn boot_map_write_combining(phys: u64, size: u64) -> bool {
    for pass in [Pass::Prove, Pass::Switch] {
        for base in [phys, crate::mm::PHYS_OFFSET + phys] {
            let mut at = base & !(PAGE_2M - 1);
            let end = base.saturating_add(size);
            while at < end {
                if !write_combine_leaf(at, pass) {
                    return false;
                }
                at += PAGE_2M;
            }
        }
    }
    true
}

/// Which half of [`boot_map_write_combining`] a walk is: the first proves
/// every leaf, the second writes the ones the first found.
#[derive(Clone, Copy, PartialEq)]
enum Pass {
    Prove,
    Switch,
}

/// Whether the current tables hold a 2 MiB leaf for `addr`, and on
/// [`Pass::Switch`] gives that leaf the write-combining type.
fn write_combine_leaf(addr: u64, pass: Pass) -> bool {
    // SAFETY: sound as `present_in_current_tables`'s walk — `Cr3::current()` names
    // the table this CPU runs under and every step is taken through a
    // `PAGE_PRESENT` entry. Exclusive because this runs on the BSP before any
    // AP exists and before `mm::init` publishes a space of its own.
    let mut table = unsafe { PageTablePage::from_phys_mut(Cr3::current().phys()) };
    let (pml4, pdpt, pd) = indices(addr);
    for index in [pml4, pdpt] {
        let entry = table[index];
        if entry & PAGE_PRESENT == 0 || entry & PAGE_SIZE_BIT != 0 {
            return false;
        }
        // SAFETY: as above — present and not a leaf, so it names a table.
        table = unsafe { PageTablePage::from_phys_mut(entry & ADDR_MASK) };
    }
    let entry = table[pd];
    if entry & PAGE_PRESENT == 0 || entry & PAGE_SIZE_BIT == 0 {
        return false;
    }
    if pass == Pass::Switch {
        let value = (entry & !(PAGE_CACHE_DISABLE | PAGE_WRITE_THROUGH))
            | CachePolicy::WriteCombining.pde_bits();
        // What a change to a live page's memory type owes this CPU's TLB
        // (SDM Vol. 3A §11.12.4), through the accounting every other writer of
        // a live paging structure in this file goes through.
        table.write_pde(pd, addr, value).discharge(Cr3::current());
    }
    true
}

/// Dump page table entries for an address. Lock-free for crash safety.
pub fn debug_page_walk(addr: u64) {
    let cr3 = Cr3::current();
    // SAFETY: same argument as `present_in_current_tables` above.
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
    // this is a table of 4 KiB leaves this file built; same reasoning as above.
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

/// What a write-combining scanout at `[addr, addr + size)` actually is, as the
/// GOP driver reports it: the effective type under PAT entry WC, the MTRRs'
/// type for the range, and the entry.
pub fn scanout_memory_type(addr: u64, size: u64) -> impl core::fmt::Display {
    struct Report(crate::arch::mtrr::Effective);
    impl core::fmt::Display for Report {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            write!(
                f,
                "{} (MTRR {}, PAT entry {})",
                crate::arch::mtrr::effective_under_wc(&self.0).map_or("unknown", |t| t.name()),
                self.0.name(),
                crate::arch::pat::WC_ENTRY,
            )
        }
    }
    Report(crate::arch::mtrr::range_type(addr, size))
}

/// Whether a process can be given the scanout at `phys`, which it is in
/// 2 MiB pages: its base on one, as the loader already requires, since what
/// the last page covers past it is the aperture firmware put it in.
pub fn scanout_whole_pages(phys: u64, _size: u64) -> Result<(), &'static str> {
    if phys.is_multiple_of(PAGE_2M) {
        Ok(())
    } else {
        Err("its base is not on a 2 MiB page, and a process is given a scanout in 2 MiB pages")
    }
}
