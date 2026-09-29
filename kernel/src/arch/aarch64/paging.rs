//! Page tables and address spaces: VMSAv8-64 stage 1 with a 4 KiB granule
//! (Arm ARM K.a, chapter D8), and the only code that writes a translation
//! table entry.
//!
//! **Two tables, not one root with two halves.** `TTBR1_EL1` walks the
//! kernel's tables — the direct map at `PHYS_OFFSET`, which every address
//! space shares because every one runs under the same `TTBR1_EL1` — and
//! `TTBR0_EL1` walks one [`AddressSpace`]'s user half, tagged with the ASID it
//! owns. A user space copies nothing of the kernel's, and switching to one is
//! one register write.
//!
//! **The direct map holds memory and nothing else**: every 4 KiB page
//! firmware's map calls memory the kernel reads (`toyos_bootmap::aarch64`),
//! Normal write-back; a device's registers only as [`map_mmio`] maps them; and
//! the scanout. A page is never retyped: a mapping that disagrees with the one
//! already there is refused by name, because one page under two memory types
//! loses coherency (D8.2.12).
//!
//! **A live entry is replaced break-before-make**: written invalid, its
//! translation dropped on every CPU (`super::tlb`), and only then written
//! again (D8.14.1). An entry that was invalid owes nothing, since no TLB holds
//! a translation that faulted.

use alloc::boxed::Box;
use alloc::vec::Vec;

use toyos_bootmap::aarch64::{coverage, direct_map_end, Coverage, ATTR_DEVICE, ATTR_NORMAL, ATTR_NORMAL_NC};
use toyos_pcid::{Alloc, Pcid, PcidPool};

use super::tlb;
use crate::mm::policy::{CachePolicy, MmioPolicy, Prot, WindowProt};
use crate::mm::{DirectMap, UserAddr, PAGE_2M, PHYS_OFFSET};
use crate::sync::{Lock, LockGuard};
use crate::vma::{self, Occupancy, Region, RegionKind};
use crate::MemoryMapEntry;

const VALID: u64 = 1 << 0;
/// At levels 0 to 2 a descriptor naming the next table, at level 3 a page;
/// clear at level 2 is a 2 MiB block.
const TABLE: u64 = 1 << 1;
const ATTR_INDEX: u64 = 0b111 << 2;
/// `AP[1]`: EL0 may access.
const AP_EL0: u64 = 1 << 6;
/// `AP[2]`: read-only at every level that may access.
const AP_READ_ONLY: u64 = 1 << 7;
const INNER_SHAREABLE: u64 = 0b11 << 8;
const OUTER_SHAREABLE: u64 = 0b10 << 8;
/// The access flag: set, so no first access faults.
const AF: u64 = 1 << 10;
/// Not global: the translation is the owning ASID's alone.
const NOT_GLOBAL: u64 = 1 << 11;
const PXN: u64 = 1 << 53;
const UXN: u64 = 1 << 54;
const ADDR: u64 = 0x0000_FFFF_FFFF_F000;
const ADDR_2M: u64 = 0x0000_FFFF_FFE0_0000;
const PAGE_4K: u64 = crate::mm::PAGE_SIZE;

/// A leaf's memory type and shareability. A device is never executable,
/// since a speculative fetch from one is a read of its registers.
fn typed(cache: CachePolicy) -> u64 {
    match cache {
        CachePolicy::Normal => ATTR_NORMAL << 2 | INNER_SHAREABLE,
        CachePolicy::Uncacheable => ATTR_DEVICE << 2 | PXN | UXN,
        CachePolicy::WriteCombining => ATTR_NORMAL_NC << 2 | OUTER_SHAREABLE,
    }
}

/// The type a leaf this file wrote names; any other index is one it never wrote.
fn policy_of(leaf: u64) -> CachePolicy {
    match (leaf & ATTR_INDEX) >> 2 {
        ATTR_NORMAL => CachePolicy::Normal,
        ATTR_DEVICE => CachePolicy::Uncacheable,
        ATTR_NORMAL_NC => CachePolicy::WriteCombining,
        index => panic!("paging: the leaf {leaf:#x} names MAIR_EL1 index {index}, which this kernel never writes"),
    }
}

impl Prot {
    /// A user leaf's permissions: EL0 reads every one, and EL1 executes none.
    fn user_bits(self) -> u64 {
        match self {
            Self::Read => AP_EL0 | AP_READ_ONLY | UXN | PXN,
            Self::ReadWrite => AP_EL0 | UXN | PXN,
            Self::ReadExec => AP_EL0 | AP_READ_ONLY | PXN,
        }
    }
}

/// A user leaf, bar its address and whether it is a block or a page.
fn user_leaf(prot: Prot, cache: CachePolicy) -> u64 {
    VALID | AF | NOT_GLOBAL | typed(cache) | prot.user_bits()
}

/// A kernel leaf: EL1 read-write, EL0 nothing, global, and executable at EL1
/// only where it is memory — the kernel runs from the direct map.
fn kernel_leaf(cache: CachePolicy) -> u64 {
    VALID | AF | typed(cache) | UXN
}

/// A leaf with its address and its block-or-page bit taken out: what two
/// mappings of one page must agree on.
fn attributes(leaf: u64) -> u64 {
    leaf & !ADDR & !TABLE
}

/// The index `va` takes at `level` (0 to 3) of a 4 KiB-granule walk.
fn index(va: u64, level: u32) -> usize {
    ((va >> (39 - 9 * level)) & 0x1FF) as usize
}

/// One translation table: 512 descriptors in one aligned page.
#[repr(C, align(4096))]
struct Table([u64; 512]);

impl Table {
    fn new() -> Box<Self> {
        Box::new(Self([0; 512]))
    }

    fn phys(&self) -> u64 {
        DirectMap::phys_of(self)
    }

    /// # Safety
    /// `phys` is a table this file built and linked, alive while the reference is.
    unsafe fn at<'a>(phys: u64) -> &'a Table {
        // SAFETY: the caller's contract.
        unsafe { &*DirectMap::from_phys(phys).as_ptr::<Table>() }
    }

    /// # Safety
    /// As [`Table::at`], and the reference is the only live one.
    unsafe fn at_mut<'a>(phys: u64) -> &'a mut Table {
        // SAFETY: the caller's contract.
        unsafe { &mut *DirectMap::from_phys(phys).as_mut_ptr::<Table>() }
    }

    /// The next table down from a valid table descriptor at `i`.
    fn child(&self, i: usize) -> Option<&Table> {
        let entry = self.0[i];
        // SAFETY: valid and a table, so it names one this file linked.
        (entry & (VALID | TABLE) == VALID | TABLE).then(|| unsafe { Table::at(entry & ADDR) })
    }

    /// Write the entry at `i`, which the hardware may be walking: one aligned
    /// store, so no walker sees half a descriptor.
    fn set(&mut self, i: usize, value: u64) {
        // SAFETY: `i` indexes this table, a live `&mut`.
        unsafe { core::ptr::write_volatile(&raw mut self.0[i], value) };
    }
}

/// Every table written reached every walker, and this CPU walks afresh.
fn published() {
    // SAFETY: barriers; they touch no memory.
    unsafe { core::arch::asm!("dsb ishst", "isb", options(nostack, preserves_flags)) };
}

/// A root and every table under it, owned together and freed together.
struct Tables {
    root: Box<Table>,
    children: Vec<Box<Table>>,
}

impl Tables {
    fn new() -> Self {
        Self { root: Table::new(), children: Vec::new() }
    }

    fn adopt(&mut self, table: Box<Table>) -> u64 {
        let phys = table.phys();
        self.children.push(table);
        phys
    }

    /// The level-2 table over `va`, built down to it where absent. Only an
    /// invalid entry is written, which owes no invalidation.
    fn directory(&mut self, va: u64) -> &mut Table {
        let mut table: *mut Table = &mut *self.root;
        for level in 0..2 {
            // SAFETY: `table` is this root or a table it linked, and
            // `&mut self` makes this the only reference.
            let current = unsafe { &mut *table };
            let i = index(va, level);
            if current.0[i] & VALID == 0 {
                let phys = self.adopt(Table::new());
                current.set(i, phys | TABLE | VALID);
            }
            let entry = current.0[i];
            assert!(entry & TABLE != 0, "paging: a block at level {level} over {va:#x}, which this kernel never writes");
            // SAFETY: valid and a table: one this root linked.
            table = unsafe { Table::at_mut(entry & ADDR) };
        }
        // SAFETY: as in the loop.
        unsafe { &mut *table }
    }

    /// The level-2 table over `va`, if the walk reaches one.
    fn find_directory(&self, va: u64) -> Option<&Table> {
        self.root.child(index(va, 0))?.child(index(va, 1))
    }

    /// The leaf that maps `va` and the size it maps, if one does.
    fn leaf(&self, va: u64) -> Option<(u64, u64)> {
        let directory = self.find_directory(va)?;
        let entry = directory.0[index(va, 2)];
        if entry & VALID == 0 {
            return None;
        }
        if entry & TABLE == 0 {
            return Some((entry, PAGE_2M));
        }
        let page = directory.child(index(va, 2))?.0[index(va, 3)];
        (page & VALID != 0).then_some((page, PAGE_4K))
    }

    /// Map `[phys, phys + size)` at `PHYS_OFFSET + phys` with `leaf`'s
    /// attributes: a whole 2 MiB page as a block where nothing maps it yet,
    /// the rest page by page. A page already mapped the same way is left as
    /// it is; one mapped any other way is refused.
    fn map_direct(&mut self, phys: u64, size: u64, leaf: u64) {
        let (start, end) = (phys & !(PAGE_4K - 1), (phys + size).next_multiple_of(PAGE_4K));
        let mut at = start;
        while at < end {
            let va = PHYS_OFFSET + at;
            let block = at & !(PAGE_2M - 1);
            let i = index(va, 2);
            let entry = self.directory(va).0[i];
            if entry & (VALID | TABLE) == VALID {
                refuse_unless_same(at, entry, leaf);
                at = block + PAGE_2M;
                continue;
            }
            if entry & VALID == 0 && at == block && block + PAGE_2M <= end {
                self.directory(va).set(i, block | leaf);
                at += PAGE_2M;
                continue;
            }
            let pages = if entry & VALID == 0 {
                let phys = self.adopt(Table::new());
                self.directory(va).set(i, phys | TABLE | VALID);
                phys
            } else {
                entry & ADDR
            };
            // SAFETY: a table this root linked, reached under `&mut self`.
            let pages = unsafe { Table::at_mut(pages) };
            let j = index(va, 3);
            if pages.0[j] & VALID != 0 {
                refuse_unless_same(at, pages.0[j], leaf);
            } else {
                pages.set(j, at | leaf | TABLE);
            }
            at += PAGE_4K;
        }
        published();
    }
}

/// A second mapping of the page at `phys` agrees with the first, or the
/// kernel stops: [`Tables::map_direct`]'s refusal.
fn refuse_unless_same(phys: u64, existing: u64, wanted: u64) {
    assert!(
        attributes(existing) == attributes(wanted),
        "paging: {phys:#x} is mapped {:?} ({existing:#x}) and cannot also be {:?}",
        policy_of(existing),
        policy_of(wanted),
    );
}

/// What `TTBR0_EL1` is loaded with for a space: its ASID in bits 63:48 and
/// its root table's address.
#[derive(Clone, Copy)]
pub struct Root(u64);

impl Root {
    pub fn phys(self) -> u64 {
        self.0 & ADDR
    }

    /// No invalidation: the ASID is this space's alone, and a returned one was
    /// dropped from every CPU before it was issued again (`toyos_pcid`).
    /// # Safety
    /// The underlying page tables must be valid and live.
    pub unsafe fn activate(self) {
        // SAFETY: the caller's contract; the `ISB` makes the next walk use it.
        unsafe { core::arch::asm!("msr ttbr0_el1, {}", "isb", in(reg) self.0, options(nostack, preserves_flags)) };
    }
}

/// The ASID allocator: `toyos_pcid`'s tags, which 16-bit ASIDs hold whole,
/// and its reclaim given the shootdown it asks for.
static ASIDS: Lock<PcidPool> = Lock::new(PcidPool::new());

/// A user ASID owned for one space's life; its drop returns it.
struct AsidGuard(Pcid);

impl Drop for AsidGuard {
    fn drop(&mut self) {
        ASIDS.lock().free(self.0);
    }
}

enum Asid {
    /// ASID 0, the kernel space's, whose user half is empty.
    Kernel,
    User(AsidGuard),
}

impl Asid {
    fn value(&self) -> u16 {
        match self {
            Self::Kernel => toyos_pcid::KERNEL_PCID,
            Self::User(guard) => guard.0.get(),
        }
    }
}

/// `None` when every user ASID is held by a live space.
fn alloc_asid() -> Option<AsidGuard> {
    let mut pool = ASIDS.lock();
    loop {
        match pool.alloc() {
            Alloc::Ready(tag) => return Some(AsidGuard(tag)),
            Alloc::NeedsFlush => {
                tlb::all(crate::invalidation::Origin::Pcid);
                pool.reclaim();
            }
            Alloc::Exhausted => return None,
        }
    }
}

/// A process's user half, walked through `TTBR0_EL1`: its tables, its
/// regions and its ASID.
pub struct AddressSpace {
    tables: Tables,
    regions: vma::Regions,
    asid: Asid,
}

impl AddressSpace {
    /// An empty user half, or `None` when every user ASID is held.
    pub fn new_user() -> Option<Self> {
        let asid = alloc_asid()?;
        Some(Self { tables: Tables::new(), regions: vma::Regions::default(), asid: Asid::User(asid) })
    }

    pub fn root(&self) -> Root {
        Root(u64::from(self.asid.value()) << 48 | self.tables.root.phys())
    }

    /// Replace the level-2 entry over `va`, break-before-make where it was
    /// valid; a table it named stays owned by this space until it drops.
    fn replace(&mut self, va: u64, value: u64) {
        let asid = self.asid.value();
        let directory = self.tables.directory(va);
        let i = index(va, 2);
        let prior = directory.0[i];
        if prior & VALID != 0 {
            directory.set(i, 0);
            if prior & TABLE != 0 {
                tlb::asid(asid);
            } else {
                tlb::page(asid, va);
            }
        }
        directory.set(i, value);
        published();
    }

    /// Empty level-2 entries (aligned, asserted) mean nothing can be stale.
    pub fn map_range(&mut self, vaddr: UserAddr, phys: u64, size: u64, prot: Prot, cache: CachePolicy) {
        assert!(vaddr.raw() & (PAGE_2M - 1) == 0, "map_range: vaddr not 2MB-aligned");
        assert!(phys & (PAGE_2M - 1) == 0, "map_range: phys {phys:#x} not 2MB-aligned");
        if prot == Prot::ReadExec {
            super::cache::make_executable(DirectMap::from_phys(phys).as_ptr::<u8>() as u64, size as usize);
        }
        let mut offset = 0u64;
        while offset < size {
            let va = vaddr.raw() + offset;
            let directory = self.tables.directory(va);
            let i = index(va, 2);
            assert!(directory.0[i] & VALID == 0, "map_range: an install at {va:#x} found the present entry {:#x}", directory.0[i]);
            directory.set(i, (phys + offset) | user_leaf(prot, cache));
            offset += PAGE_2M;
        }
        published();
    }

    fn unmap_range(&mut self, vaddr: UserAddr, size: u64) {
        let mut offset = 0u64;
        while offset < size {
            self.unmap(UserAddr::new(vaddr.raw() + offset));
            offset += PAGE_2M;
        }
    }

    /// Replaces whatever maps `vaddr`, in this space only.
    pub fn remap(&mut self, vaddr: UserAddr, phys: u64, prot: Prot) {
        let va = vaddr.raw();
        assert!(va & (PAGE_2M - 1) == 0, "remap: vaddr {va:#x} not 2MB-aligned");
        assert!(phys & (PAGE_2M - 1) == 0, "remap: phys {phys:#x} not 2MB-aligned");
        if prot == Prot::ReadExec {
            super::cache::make_executable(DirectMap::from_phys(phys).as_ptr::<u8>() as u64, PAGE_2M as usize);
        }
        self.replace(va, phys | user_leaf(prot, CachePolicy::Normal));
    }

    /// A mixed window's table is filled before it is linked, so nothing walks
    /// it half-written. Must not be called twice on one address.
    pub fn map_window(&mut self, vaddr: UserAddr, phys: u64, prot: &WindowProt) {
        if let Some(uniform) = prot.agreed() {
            self.remap(vaddr, phys, uniform);
            return;
        }
        let va = vaddr.raw();
        assert!(va & (PAGE_2M - 1) == 0, "map_window: vaddr {va:#x} not 2MB-aligned");
        assert!(phys & (PAGE_2M - 1) == 0, "map_window: phys {phys:#x} not 2MB-aligned");
        let mut table = Table::new();
        for (i, page_prot) in prot.pages().enumerate() {
            let at = phys + i as u64 * PAGE_4K;
            if page_prot == Prot::ReadExec {
                super::cache::make_executable(DirectMap::from_phys(at).as_ptr::<u8>() as u64, PAGE_4K as usize);
            }
            table.0[i] = at | user_leaf(page_prot, CachePolicy::Normal) | TABLE;
        }
        let table = self.tables.adopt(table);
        self.replace(va, table | TABLE | VALID);
    }

    /// `false` leaves the mapping as found and `phys` the caller's to free:
    /// the check and the write are one critical section under the caller's lock.
    pub fn map_window_if_absent(&mut self, vaddr: UserAddr, phys: u64, prot: &WindowProt) -> bool {
        if self.translate(vaddr).is_some() {
            return false;
        }
        self.map_window(vaddr, phys, prot);
        true
    }

    /// Ends any futex wait on the frame (its token is a physical address)
    /// before the frame can reach the PMM and be reissued under a waiter.
    pub fn unmap(&mut self, vaddr: UserAddr) {
        let va = vaddr.raw();
        assert!(va & (PAGE_2M - 1) == 0, "unmap: vaddr {va:#x} not 2MB-aligned");
        let asid = self.asid.value();
        let Some(directory) = self.tables.find_directory(va) else { return };
        let i = index(va, 2);
        let entry = directory.0[i];
        if entry & VALID == 0 {
            return;
        }
        // A split window's entries all address one 2 MiB frame, so its first names it.
        let phys = match directory.child(i) {
            Some(pages) => pages.0[0] & ADDR_2M,
            None => entry & ADDR_2M,
        };
        self.tables.directory(va).set(i, 0);
        if entry & TABLE != 0 {
            tlb::asid(asid);
        } else {
            tlb::page(asid, va);
        }
        crate::sched::futex::revoke_range(phys, PAGE_2M);
    }

    /// Checked here, not at the callers: only a user address names user memory.
    pub fn translate(&self, vaddr: UserAddr) -> Option<DirectMap> {
        self.walk(vaddr).map(|(at, _)| at)
    }

    /// As [`translate`](Self::translate), but only where an EL0 store would
    /// land: a leaf EL0 may write. A kernel copy into user memory goes
    /// through this, so a syscall cannot write a page the process itself may
    /// not — the clock page, a shared library's `.text`.
    pub fn translate_writable(&self, vaddr: UserAddr) -> Option<DirectMap> {
        self.walk(vaddr).and_then(|(at, leaf)| (leaf & (AP_EL0 | AP_READ_ONLY) == AP_EL0).then_some(at))
    }

    fn walk(&self, vaddr: UserAddr) -> Option<(DirectMap, u64)> {
        let va = vaddr.raw();
        if !toyos_userbound::is_user_addr(va) {
            return None;
        }
        let (leaf, size) = self.tables.leaf(va)?;
        let base = leaf & if size == PAGE_2M { ADDR_2M } else { ADDR };
        Some((DirectMap::from_phys(base + (va & (size - 1))), leaf))
    }

    pub fn alloc_region(&mut self, size: u64, kind: RegionKind) -> Option<UserAddr> {
        self.regions.alloc(size, kind)
    }

    pub fn alloc_and_map(&mut self, phys: u64, size: u64, prot: Prot, cache: CachePolicy) -> Option<(UserAddr, u64)> {
        assert!(phys & (PAGE_2M - 1) == 0, "alloc_and_map: phys {phys:#x} not 2MB-aligned");
        let (addr, aligned) = self.regions.alloc_mapped(size)?;
        self.map_range(addr, phys, aligned, prot, cache);
        Some((addr, aligned))
    }

    pub fn free_and_unmap(&mut self, addr: UserAddr) -> Option<u64> {
        let size = self.regions.remove(addr)?;
        self.unmap_range(addr, size);
        Some(size)
    }

    pub fn insert_region(&mut self, addr: UserAddr, region: Region) {
        self.regions.insert(addr, region);
    }

    pub fn find_region(&self, addr: UserAddr) -> Option<(UserAddr, &Region)> {
        self.regions.find(addr)
    }

    pub fn occupancy(&self, addr: UserAddr, size: u64) -> Occupancy {
        self.regions.occupancy(addr, size)
    }

    pub fn overlapping_regions(&self, start: UserAddr, end: UserAddr) -> impl Iterator<Item = (&UserAddr, &Region)> {
        self.regions.overlapping(start, end)
    }

    /// The direct map's type for `phys`, read off the kernel's tables, which
    /// every space shares.
    pub fn direct_map_policy(&self, phys: u64) -> Option<CachePolicy> {
        high().as_ref()?.leaf(DirectMap::from_phys(phys).as_ptr::<u8>() as u64).map(|(leaf, _)| policy_of(leaf))
    }

    pub fn user_policy(&self, addr: UserAddr) -> Option<CachePolicy> {
        self.tables.leaf(addr.raw()).map(|(leaf, _)| policy_of(leaf))
    }

    /// Take the 4 KiB page at `phys` out of the direct map, for good: the
    /// caller owns it for the machine's life. The 2 MiB page around it is
    /// split break-before-make, so it must be one nothing is running from.
    pub fn guard_4k(&mut self, phys: u64) {
        assert!(phys & (PAGE_4K - 1) == 0, "guard_4k: phys {phys:#x} not 4 KiB-aligned");
        let va = DirectMap::from_phys(phys).as_ptr::<u8>() as u64;
        let mut high = high();
        let high = high.as_mut().expect("guard_4k: before the kernel's tables exist");
        let i = index(va, 2);
        let entry = high.directory(va).0[i];
        assert!(entry & VALID != 0, "guard_4k: {phys:#x} is not in the direct map");
        if entry & TABLE == 0 {
            let base = entry & ADDR_2M;
            let mut pages = Table::new();
            for (j, page) in pages.0.iter_mut().enumerate() {
                *page = (base + j as u64 * PAGE_4K) | attributes(entry) | TABLE;
            }
            let pages = high.adopt(pages);
            let directory = high.directory(va);
            directory.set(i, 0);
            tlb::kernel_page(va);
            directory.set(i, pages | TABLE | VALID);
        }
        let pages = high.directory(va).0[i] & ADDR;
        // SAFETY: the table the kernel's root linked, just above or before.
        let pages = unsafe { Table::at_mut(pages) };
        let j = index(va, 3);
        assert!(pages.0[j] & VALID != 0, "guard_4k: {phys:#x} is already unmapped");
        pages.set(j, 0);
        tlb::kernel_page(va);
    }
}

/// The kernel's own tables, `TTBR1_EL1`'s.
static HIGH: Lock<Option<Tables>> = Lock::new(None);

fn high() -> LockGuard<'static, Option<Tables>> {
    HIGH.lock()
}

/// The kernel's address space: the empty user half a kernel thread and an
/// idle CPU run under, ASID 0. Leaked, since it outlives every task.
static KERNEL: core::sync::atomic::AtomicPtr<alloc::sync::Arc<Lock<AddressSpace>>> =
    core::sync::atomic::AtomicPtr::new(core::ptr::null_mut());

/// Its root, cached for lock-free access from panic and crash paths.
static KERNEL_TTBR0: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

pub fn kernel() -> &'static alloc::sync::Arc<Lock<AddressSpace>> {
    let ptr = KERNEL.load(core::sync::atomic::Ordering::Acquire);
    assert!(!ptr.is_null(), "paging not initialized");
    // SAFETY: written once in `init` with the `Release` this pairs with, never
    // cleared, so the pointer is live for the machine's life.
    unsafe { &*ptr }
}

pub fn kernel_root() -> Root {
    Root(KERNEL_TTBR0.load(core::sync::atomic::Ordering::Relaxed))
}

/// Leave whatever user half is current for the kernel's empty one.
pub fn activate_kernel() {
    // SAFETY: the kernel space's root is an empty table that lives forever.
    unsafe { kernel_root().activate() };
}

/// Map a device's registers, or the scanout, into the direct map: 4 KiB pages
/// exactly over `[phys, phys + size)`, a block where a whole 2 MiB page is
/// asked for and free. No invalidation is owed, since only an invalid entry is
/// ever written.
pub fn map_mmio(phys: u64, size: u64, policy: MmioPolicy) -> crate::mm::Mmio {
    high().as_mut().expect("map_mmio: before the kernel's tables exist").map_direct(phys, size, kernel_leaf(policy.cache()));
    let installed = kernel().lock().direct_map_policy(phys).expect("map_mmio: the window was just mapped");
    assert!(installed == policy.cache(), "map_mmio: {phys:#x} installed {installed:?}");
    crate::log!("mmio: {phys:#x}+{size:#x} {installed:?}");
    crate::mm::Mmio::new(DirectMap::from_phys(phys), size)
}

/// Build the kernel's tables — every page of memory firmware's map names,
/// and the console UART and the scanout the boot's own records reach —
/// switch `TTBR1_EL1` to them and `TTBR0_EL1` to the kernel's empty user
/// half, and answer where the direct map ends.
pub(crate) fn init(memory_map: &[MemoryMapEntry], scanout: Option<(u64, u64)>) -> toyos_bootmap::DirectMapEnd {
    let extent = direct_map_end(memory_map).unwrap_or_else(|refusal| panic!("paging: firmware's memory map: {refusal}"));
    let end = extent.get();
    let memory = kernel_leaf(CachePolicy::Normal);
    let mut tables = Tables::new();
    let (mut blocks, mut pages) = (0u64, 0u64);
    let mut at = 0;
    while at < end {
        let va = PHYS_OFFSET + at;
        match coverage(memory_map, at) {
            Coverage::Nothing => {}
            Coverage::Whole => {
                tables.directory(va).set(index(va, 2), at | memory);
                blocks += 1;
            }
            held @ Coverage::Pages(_) => {
                let mut table = Table::new();
                for j in (0..512).filter(|&j| held.holds(j)) {
                    table.0[j as usize] = (at + j * PAGE_4K) | memory | TABLE;
                    pages += 1;
                }
                let table = tables.adopt(table);
                tables.directory(va).set(index(va, 2), table | TABLE | VALID);
            }
        }
        at += PAGE_2M;
    }
    if let Some(uart) = super::console_uart::frame() {
        tables.map_direct(uart, PAGE_4K, kernel_leaf(CachePolicy::Uncacheable));
    }
    if let Some((scanout, size)) = scanout {
        tables.map_direct(scanout, size, kernel_leaf(CachePolicy::WriteCombining));
    }

    let ttbr1 = tables.root.phys();
    *high() = Some(tables);
    let space = AddressSpace { tables: Tables::new(), regions: vma::Regions::default(), asid: Asid::Kernel };
    let ttbr0 = space.root();
    KERNEL_TTBR0.store(ttbr0.0, core::sync::atomic::Ordering::Release);
    let published: &'static alloc::sync::Arc<Lock<AddressSpace>> =
        Box::leak(Box::new(alloc::sync::Arc::new(Lock::new(space))));
    KERNEL.store(published as *const _ as *mut _, core::sync::atomic::Ordering::Release);

    // SAFETY: both tables are built and published above, and the new
    // `TTBR1_EL1` maps the code, stack and data this runs on exactly as the
    // loader's did; the loader's identity view goes with the old `TTBR0_EL1`,
    // and the local `TLBI` drops every translation either left behind.
    unsafe {
        core::arch::asm!(
            "dsb ishst",
            "msr ttbr1_el1, {ttbr1}",
            "msr ttbr0_el1, {ttbr0}",
            "isb",
            "tlbi vmalle1",
            "dsb nsh",
            "isb",
            ttbr1 = in(reg) ttbr1,
            ttbr0 = in(reg) ttbr0.0,
            options(nostack, preserves_flags),
        );
    }
    crate::log!("paging: the direct map holds memory below {end:#x} in {blocks} 2 MiB blocks and {pages} 4 KiB pages");
    extent
}

/// Nothing to seal: every space runs under the one `TTBR1_EL1`, so no space
/// holds a copy of the kernel's root slots to fall behind.
pub(crate) fn seal_kernel_half() {}

/// `PAR_EL1` after the MMU translated `addr` for an EL1 read in the tables
/// this CPU runs on now (`AT S1E1R`; Arm ARM K.a, C6.2.x and D24.2.131):
/// bit 0 set is a fault, and otherwise bits 63:56 are the memory type.
fn translate_read(addr: u64) -> u64 {
    let par: u64;
    // SAFETY: `AT` translates without accessing memory and reports through
    // `PAR_EL1`; the `ISB` makes the result the one read back.
    unsafe {
        core::arch::asm!(
            "at s1e1r, {addr}",
            "isb",
            "mrs {par}, par_el1",
            addr = in(reg) addr,
            par = out(reg) par,
            options(nostack, preserves_flags),
        );
    }
    par
}

/// Whether `addr` translates for a read in the tables this CPU runs on now:
/// the MMU's own answer, so broken tables become "not present" and never a
/// fault here.
pub fn present_in_current_tables(addr: u64) -> bool {
    translate_read(addr) & 1 == 0
}

/// Whether the scanout is write-combining at both of its addresses already:
/// on AArch64 that is Normal non-cacheable, whose stores gather, and the
/// loader maps it so. Asked of the MMU page by page rather than assumed, and
/// nothing is retyped: the type the loader wrote is the final one.
pub fn boot_map_write_combining(phys: u64, size: u64) -> bool {
    [phys, PHYS_OFFSET + phys].iter().all(|&base| {
        (0..size.div_ceil(4096)).all(|page| {
            let par = translate_read(base + page * 4096);
            // Outer Normal non-cacheable, and an inner nibble that says the same:
            // 0b0100 as `MAIR_EL1` spells it, or 0b0000, which is how a CPU may
            // report an inner Non-cacheable attribute in `PAR_EL1` (HVF on Apple
            // silicon does), and which `MAIR_EL1` itself never holds for Normal.
            let (outer, inner) = ((par >> 60) & 0xF, (par >> 56) & 0xF);
            par & 1 == 0 && outer == 0b0100 && (inner == 0b0100 || inner == 0)
        })
    })
}

/// What memory type the scanout is mapped with, for the GPU's report: the
/// MMU's own answer for its first byte in the direct map.
pub fn scanout_memory_type(addr: u64, _size: u64) -> impl core::fmt::Display {
    struct Report(u64);
    impl core::fmt::Display for Report {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            match self.0 & 1 {
                0 => write!(f, "PAR_EL1.ATTR {:#04x}", self.0 >> 56),
                _ => write!(f, "untranslated (PAR_EL1 {:#x})", self.0),
            }
        }
    }
    Report(translate_read(PHYS_OFFSET + addr))
}

/// Each level's descriptor for `addr` in the tables this CPU runs under now,
/// read without a lock for a crash report.
pub fn debug_page_walk(addr: u64) {
    let (name, ttbr) = if addr >> 63 == 0 {
        let ttbr: u64;
        // SAFETY: reads `TTBR0_EL1`.
        unsafe { core::arch::asm!("mrs {}, ttbr0_el1", out(reg) ttbr, options(nomem, nostack, preserves_flags)) };
        ("TTBR0_EL1", ttbr)
    } else {
        let ttbr: u64;
        // SAFETY: reads `TTBR1_EL1`.
        unsafe { core::arch::asm!("mrs {}, ttbr1_el1", out(reg) ttbr, options(nomem, nostack, preserves_flags)) };
        ("TTBR1_EL1", ttbr)
    };
    crate::log!("  Page walk for {addr:#x} through {name}={ttbr:#x}:");
    let mut table = ttbr & ADDR;
    for level in 0..4 {
        // SAFETY: a table the live translation register or a valid table
        // descriptor above it names, which the MMU is walking now.
        let entry = unsafe { Table::at(table) }.0[index(addr, level)];
        crate::log!("    L{level}[{}] = {entry:#018x}", index(addr, level));
        if entry & VALID == 0 || level == 3 || entry & TABLE == 0 {
            return;
        }
        table = entry & ADDR;
    }
}

/// The word at user address `addr` in the space this CPU runs under now,
/// read through the direct map by a walk that takes no lock and faults
/// nowhere: for a crash report, which may hold any lock already.
pub(super) fn read_user_word(addr: u64) -> Option<u64> {
    if !addr.is_multiple_of(8) || !toyos_userbound::is_user_addr(addr) {
        return None;
    }
    let ttbr0: u64;
    // SAFETY: reads `TTBR0_EL1`.
    unsafe { core::arch::asm!("mrs {}, ttbr0_el1", out(reg) ttbr0, options(nomem, nostack, preserves_flags)) };
    let mut table = ttbr0 & ADDR;
    for level in 0..4 {
        // SAFETY: the live `TTBR0_EL1`'s table or one a valid table
        // descriptor above names, which the MMU walks now.
        let entry = unsafe { Table::at(table) }.0[index(addr, level)];
        if entry & VALID == 0 {
            return None;
        }
        let size = match (level, entry & TABLE != 0) {
            (3, _) => PAGE_4K,
            (2, false) => PAGE_2M,
            (_, true) => {
                table = entry & ADDR;
                continue;
            }
            (_, false) => return None,
        };
        let base = entry & if size == PAGE_2M { ADDR_2M } else { ADDR };
        // SAFETY: a byte of a frame a valid leaf maps, reached through the direct map.
        return Some(unsafe { core::ptr::read_volatile(DirectMap::from_phys(base + (addr & (size - 1))).as_ptr::<u64>()) });
    }
    None
}

/// Whether a process can be given the scanout at `[phys, phys + size)`,
/// which it is in 2 MiB pages: its base on one, and nothing the last page
/// covers past it memory the direct map holds — firmware carves a scanout
/// like `ramfb`'s out of RAM, and the rest of that page is somebody else's.
pub fn scanout_whole_pages(phys: u64, size: u64) -> Result<(), &'static str> {
    if !phys.is_multiple_of(PAGE_2M) {
        return Err("its base is not on a 2 MiB page, and a process is given a scanout in 2 MiB pages");
    }
    let high = high();
    let tables = high.as_ref().expect("scanout_whole_pages: before the kernel's tables exist");
    let mut at = (phys + size).next_multiple_of(PAGE_4K);
    while at < (phys + size).next_multiple_of(PAGE_2M) {
        if tables.leaf(PHYS_OFFSET + at).is_some_and(|(leaf, _)| policy_of(leaf) == CachePolicy::Normal) {
            return Err("its last 2 MiB page holds memory past it, which a process given the page would reach");
        }
        at += PAGE_4K;
    }
    Ok(())
}
