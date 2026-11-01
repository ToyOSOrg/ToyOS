//! Page tables and address spaces: VMSAv8-64 stage 1 with a 4 KiB granule
//! (Arm ARM K.a, chapter D8), and the only code that writes a translation
//! table entry.
//!
//! **Two tables, not one root with two halves.** `TTBR1_EL1` walks the
//! kernel's tables — the direct map at `PHYS_OFFSET`, which every address
//! space shares because every one runs under the same `TTBR1_EL1` — and
//! `TTBR0_EL1` walks one [`UserTables`]' user half, tagged with the ASID it
//! owns. A user space copies nothing of the kernel's, and switching to one is
//! one register write.
//!
//! **The direct map holds memory and nothing else**: every 4 KiB page
//! firmware's map calls memory the kernel reads (`toyos_bootmap::aarch64`),
//! Normal write-back; a device's registers only as [`map_mmio`] maps them; and
//! the scanout. A page is never retyped: a mapping that disagrees with the one
//! already there is refused by name, because one page under two memory types
//! loses coherency (D8.2.12). So the kernel reads and writes a user page
//! through the direct map only where it is memory: a user leaf of a device's
//! registers answers no translation, no syscall's copy and no crash report.
//!
//! **A live entry is replaced break-before-make**: written invalid, its
//! translation dropped on every CPU (`super::tlb`), and only then written
//! again (D8.14.1). An entry that was invalid owes nothing, since no TLB holds
//! a translation that faulted.
//!
//! **No kernel leaf is both writable and executable**, and only the image's
//! text is executable (`mm::audit_wx`).


use toyos_bootmap::aarch64::{coverage, direct_map_end, Coverage, ATTR_DEVICE, ATTR_NORMAL, ATTR_NORMAL_NC};
use alloc::sync::Arc;
use kernel::pcid::{Alloc, Pcid, PcidPool};
use kernel::sched::job::Job;

use super::tlb;
use crate::mm::policy::{CachePolicy, MmioPolicy, Prot};
use crate::mm::{DirectMap, PAGE_2M, PHYS_OFFSET};
use crate::sync::{Lock, LockGuard};
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

/// Whether the frame a user leaf maps is one the kernel may reach through the
/// direct map: not a device's registers, which the direct map holds only
/// where [`map_mmio`] mapped them, and then as a type `memcpy`'s unaligned
/// access faults on.
fn through_direct_map(leaf: u64) -> bool {
    policy_of(leaf) != CachePolicy::Uncacheable
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

impl Prot {
    /// A kernel leaf's permissions: EL0 has none, and EL1 executes only code.
    fn kernel_bits(self) -> u64 {
        match self {
            Self::Read => AP_READ_ONLY | PXN,
            Self::ReadWrite => PXN,
            Self::ReadExec => AP_READ_ONLY,
        }
    }
}

/// A kernel leaf, bar its address and whether it is a block or a page: global.
fn kernel_leaf(cache: CachePolicy, prot: Prot) -> u64 {
    VALID | AF | typed(cache) | prot.kernel_bits() | UXN
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
    /// An empty table in a frame of its own, owned from here by the
    /// descriptor that links it.
    fn new() -> &'static mut Self {
        let frame = crate::mm::pmm::alloc_frame().expect("paging: no frame for a translation table");
        // SAFETY: a zeroed frame just allocated: a table of invalid
        // descriptors nothing else names.
        unsafe { Table::at_mut(frame.into_raw()) }
    }

    /// A table of a user space, counted to `job`, or `None` where taking a
    /// frame would cut into what is held back (`pmm::alloc_table_frame`).
    fn try_new(job: &Job) -> Option<&'static mut Self> {
        let phys = crate::mm::pmm::alloc_table_frame(job)?;
        // SAFETY: a zeroed frame just allocated: a table of invalid
        // descriptors nothing else names.
        Some(unsafe { Table::at_mut(phys) })
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

/// Give back a table of `job`'s user space [`Table::try_new`] made, which
/// nothing links any more.
fn free_table(job: &Job, phys: u64) {
    // SAFETY: the caller unlinked the table, after the shootdown that let
    // every CPU stop walking it.
    unsafe { crate::mm::pmm::free_held(job, [phys]) };
}

/// A root and every table under it, each a frame the descriptor linking it
/// owns.
struct Tables {
    root: u64,
}

impl Tables {
    fn new() -> Self {
        Self { root: Table::new().phys() }
    }

    fn root(&self) -> &Table {
        // SAFETY: the root is a table these tables made and own.
        unsafe { Table::at(self.root) }
    }

    fn root_mut(&mut self) -> &mut Table {
        // SAFETY: as `root`; `&mut self` gives the exclusivity.
        unsafe { Table::at_mut(self.root) }
    }

    /// The level-2 table over `va`, built down to it where absent. Only an
    /// invalid entry is written, which owes no invalidation.
    fn directory(&mut self, va: u64) -> &mut Table {
        let mut table: *mut Table = self.root_mut();
        for level in 0..2 {
            // SAFETY: `table` is this root or a table it linked, and
            // `&mut self` makes this the only reference.
            let current = unsafe { &mut *table };
            let i = index(va, level);
            if current.0[i] & VALID == 0 {
                current.set(i, Table::new().phys() | TABLE | VALID);
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
        self.root().child(index(va, 0))?.child(index(va, 1))
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
                let phys = Table::new().phys();
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
    /// dropped from every CPU before it was issued again.
    /// # Safety
    /// The underlying page tables must be valid and live.
    pub unsafe fn activate(self) {
        // SAFETY: the caller's contract; the `ISB` makes the next walk use it.
        unsafe { core::arch::asm!("msr ttbr0_el1, {}", "isb", in(reg) self.0, options(nostack, preserves_flags)) };
    }
}

/// The ASID allocator: `kernel::pcid`'s tags, which 16-bit ASIDs hold whole,
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
            Self::Kernel => kernel::pcid::KERNEL_PCID,
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

/// Take the 4 KiB page at `phys` out of the direct map, for good: the
/// caller owns it for the machine's life. The 2 MiB page around it is
/// split break-before-make, so it must be one nothing is running from.
pub fn guard_4k(phys: u64) {
    assert!(phys & (PAGE_4K - 1) == 0, "guard_4k: phys {phys:#x} not 4 KiB-aligned");
    let va = DirectMap::from_phys(phys).as_ptr::<u8>() as u64;
    let mut high = high();
    let high = high.as_mut().expect("guard_4k: before the kernel's tables exist");
    let i = index(va, 2);
    let entry = high.directory(va).0[i];
    assert!(entry & VALID != 0, "guard_4k: {phys:#x} is not in the direct map");
    if entry & TABLE == 0 {
        let base = entry & ADDR_2M;
        let pages = Table::new();
        for (j, page) in pages.0.iter_mut().enumerate() {
            *page = (base + j as u64 * PAGE_4K) | attributes(entry) | TABLE;
        }
        let pages = pages.phys();
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

/// The direct map's type for `phys`, read off the kernel's tables, which
/// every space shares.
pub fn direct_map_policy(phys: u64) -> Option<CachePolicy> {
    high().as_ref()?.leaf(DirectMap::from_phys(phys).as_ptr::<u8>() as u64).map(|(leaf, _)| policy_of(leaf))
}

/// The kernel wrote instructions into the frame at `phys` that a user leaf
/// executes: the data it wrote is cleaned to the point of unification and the
/// stale instructions invalidated, as the Arm ARM requires before they run.
pub fn wrote_code(phys: u64) {
    super::cache::make_executable(DirectMap::from_phys(phys).as_ptr::<u8>() as u64, PAGE_4K as usize);
}

/// Marks a user leaf whose frame the space owns: one of the bits [58:55] a
/// descriptor leaves to software (Arm ARM K.a, D8.3.2). Kept on a leaf that is
/// not valid, so a frame no access may reach is still recorded.
const OWNED: u64 = 1 << 55;

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
            Leaf::Mapped(m) => m.phys | user_leaf(m.prot, m.cache) | TABLE | if m.owned { OWNED } else { 0 },
        }
    }

    fn decode(descriptor: u64) -> Self {
        if descriptor & VALID == 0 {
            return if descriptor & OWNED != 0 { Leaf::Held(descriptor & ADDR) } else { Leaf::Empty };
        }
        let prot = match (descriptor & AP_READ_ONLY != 0, descriptor & UXN != 0) {
            (false, _) => Prot::ReadWrite,
            (true, true) => Prot::Read,
            (true, false) => Prot::ReadExec,
        };
        Leaf::Mapped(Mapping {
            phys: descriptor & ADDR,
            prot,
            owned: descriptor & OWNED != 0,
            cache: policy_of(descriptor),
        })
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
/// Every write reaches the walkers before it returns.
pub struct LeafTable<'a> {
    base: u64,
    table: &'a mut Table,
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
        self.write(slot, leaf);
        published();
    }

    /// Write `fill`'s leaf into each empty leaf of `[start, end)`, all of
    /// whose pages these leaves cover.
    pub fn fill(&mut self, start: u64, end: u64, mut fill: impl FnMut(u64) -> Leaf) {
        assert!(start < end && self.covers(start) && self.covers(end - 1), "paging: a fill of {start:#x}..{end:#x}");
        for i in slots(start, end, self.base, 12) {
            if Leaf::decode(self.table.0[i]) == Leaf::Empty {
                let leaf = fill(self.base + ((i as u64) << 12));
                self.write(i, leaf);
            }
        }
        published();
    }

    fn write(&mut self, slot: usize, leaf: Leaf) {
        if let Leaf::Mapped(Mapping { phys, prot: Prot::ReadExec, .. }) = leaf {
            super::cache::make_executable(DirectMap::from_phys(phys).as_ptr::<u8>() as u64, PAGE_4K as usize);
        }
        self.table.set(slot, leaf.encode());
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
        let table = unsafe { Table::at_mut(phys) };
        assert!(table.0.iter().all(|&e| e == 0), "paging: a table unlinked at {phys:#x} still holds an entry");
        table.set(0, self.head);
        self.head = phys;
    }

    /// Free the tables of `job`'s space, only after the shootdown that
    /// followed the unlink.
    fn free(self, job: &Job) {
        let mut at = self.head;
        while at != 0 {
            // SAFETY: a table of the chain, which only this frees.
            let next = unsafe { Table::at_mut(at) }.0[0];
            free_table(job, at);
            at = next;
        }
    }
}

/// One user space's tables, walked through `TTBR0_EL1` and tagged with the
/// ASID it owns. `mm::space` decides what every leaf holds; this only reads
/// and writes them. A write here owes no invalidation when the entry was
/// invalid, and the caller's [`shoot_down`](Self::shoot_down) otherwise.
pub struct UserTables {
    tables: Tables,
    asid: Asid,
    /// The job its tables are counted to.
    job: Arc<Job>,
}

impl UserTables {
    /// An empty user half, or `None` when every user ASID is held or no
    /// frame is free for its root. Its tables are counted to `job`.
    pub fn new(job: &Arc<Job>) -> Option<Self> {
        let root = Table::try_new(job)?.phys();
        let Some(asid) = alloc_asid() else {
            free_table(job, root);
            return None;
        };
        Some(Self { tables: Tables { root }, asid: Asid::User(asid), job: Arc::clone(job) })
    }

    /// The kernel's own space, whose user half is empty: what a kernel thread
    /// and an idle CPU run under. Never dropped.
    pub fn kernel() -> Self {
        let root = KERNEL_TTBR0.load(core::sync::atomic::Ordering::Acquire) & ADDR;
        assert!(root != 0, "paging: the kernel's space before `init`");
        Self { tables: Tables { root }, asid: Asid::Kernel, job: Arc::clone(crate::mm::charge::boot()) }
    }

    pub fn root(&self) -> Root {
        Root(u64::from(self.asid.value()) << 48 | self.tables.root)
    }

    /// The table of leaves over `va`, if one is built.
    fn leaves(&self, va: u64) -> Option<&Table> {
        self.tables.find_directory(va)?.child(index(va, 2))
    }

    /// Build every table `[start, end)`'s leaves need; answers how many
    /// frames that took, or `None` where a frame ran out, leaving what it
    /// built linked for an [`unlink`](Self::unlink). Only an invalid entry is
    /// written, which owes nothing.
    pub fn build(&mut self, start: u64, end: u64) -> Option<u64> {
        let job = Arc::clone(&self.job);
        let mut built = 0;
        let mut va = start & !(PAGE_2M - 1);
        while va < end {
            let mut level = self.tables.root;
            for depth in 0..3 {
                // SAFETY: this space's root or a table it linked; `&mut self`.
                let table = unsafe { Table::at_mut(level) };
                let i = index(va, depth);
                if table.0[i] & VALID == 0 {
                    let Some(child) = Table::try_new(&job) else {
                        published();
                        return None;
                    };
                    table.set(i, child.phys() | TABLE | VALID);
                    built += 1;
                }
                level = table.0[i] & ADDR;
            }
            va += PAGE_2M;
        }
        published();
        Some(built)
    }

    /// The leaf over `va`, empty where no table holds one.
    pub fn get(&self, va: u64) -> Leaf {
        self.leaves(va).map_or(Leaf::Empty, |t| Leaf::decode(t.0[index(va, 3)]))
    }

    /// Write the leaf over `va`, whose table [`build`](Self::build) made.
    pub fn set(&mut self, va: u64, leaf: Leaf) {
        self.leaf_table(va).unwrap_or_else(|| panic!("paging: no table over {va:#x}")).set(va, leaf);
    }

    /// The table of leaves over `va`'s 2 MiB, if [`build`](Self::build) made
    /// one: every leaf of it read and written for one walk.
    pub fn leaf_table(&mut self, va: u64) -> Option<LeafTable<'_>> {
        assert!(toyos_userbound::is_user_addr(va), "paging: a user leaf at {va:#x}");
        let table = self.leaves(va).map(Table::phys)?;
        // SAFETY: a table of leaves this space linked; `&mut self`.
        Some(LeafTable { base: va & !(PAGE_2M - 1), table: unsafe { Table::at_mut(table) } })
    }

    /// Call `f` on every leaf in `[start, end)` that is not empty, with its
    /// address; a `Some` it answers is written in its place.
    pub fn edit(&mut self, start: u64, end: u64, mut f: impl FnMut(u64, Leaf) -> Option<Leaf>) {
        self.each_table(start, end, |base, table| {
            for i in slots(start, end, base, 12) {
                let leaf = Leaf::decode(table.0[i]);
                if leaf != Leaf::Empty {
                    if let Some(new) = f(base + ((i as u64) << 12), leaf) {
                        if let Leaf::Mapped(Mapping { phys, prot: Prot::ReadExec, .. }) = new {
                            super::cache::make_executable(DirectMap::from_phys(phys).as_ptr::<u8>() as u64, PAGE_4K as usize);
                        }
                        table.set(i, new.encode());
                    }
                }
            }
        });
        published();
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
                    table.set(slot[k], Leaf::Empty.encode());
                }
            }
        });
        published();
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
                    table.set(i, Leaf::Empty.encode());
                    frames[n] = phys;
                    n += 1;
                }
            }
            if n > 0 {
                taken(&mut frames[..n]);
            }
        });
        published();
    }

    fn each_table(&mut self, start: u64, end: u64, mut f: impl FnMut(u64, &mut Table)) {
        let root = self.tables.root;
        // SAFETY: this space's root, and every table below it one it linked;
        // `&mut self` gives the exclusivity.
        let root = unsafe { Table::at_mut(root) };
        for a in slots(start, end, 0, 39) {
            let base_a = (a as u64) << 39;
            let Some(l1) = root.child(a).map(Table::phys) else { continue };
            // SAFETY: as above.
            let l1 = unsafe { Table::at_mut(l1) };
            for b in slots(start, end, base_a, 30) {
                let base_b = base_a + ((b as u64) << 30);
                let Some(l2) = l1.child(b).map(Table::phys) else { continue };
                // SAFETY: as above.
                let l2 = unsafe { Table::at_mut(l2) };
                for c in slots(start, end, base_b, 21) {
                    if let Some(l3) = l2.child(c).map(Table::phys) {
                        // SAFETY: as above.
                        f(base_b + ((c as u64) << 21), unsafe { Table::at_mut(l3) });
                    }
                }
            }
        }
    }

    /// Unlink every table over `[start, end)` whose span `needed` answers
    /// `false` for, and every table above one that is then empty. A table
    /// unlinked holds no leaf: a span no range needs maps nothing. They are
    /// freed by [`free`](Self::free), after the caller's shootdown.
    /// Free tables [`unlink`](Self::unlink) took out, only after the
    /// shootdown that followed it.
    pub fn free(&self, unlinked: Unlinked) {
        unlinked.free(&self.job);
    }

    pub fn unlink(&mut self, start: u64, end: u64, needed: impl Fn(u64, u64) -> bool) -> Unlinked {
        let mut unlinked = Unlinked::none();
        // SAFETY: as `each_table`'s.
        let root = unsafe { Table::at_mut(self.tables.root) };
        for a in slots(start, end, 0, 39) {
            let base_a = (a as u64) << 39;
            let Some(l1) = root.child(a).map(Table::phys) else { continue };
            // SAFETY: as above.
            let l1 = unsafe { Table::at_mut(l1) };
            for b in slots(start, end, base_a, 30) {
                let base_b = base_a + ((b as u64) << 30);
                let Some(l2) = l1.child(b).map(Table::phys) else { continue };
                // SAFETY: as above.
                let l2 = unsafe { Table::at_mut(l2) };
                for c in slots(start, end, base_b, 21) {
                    let span = base_b + ((c as u64) << 21);
                    if let Some(l3) = l2.child(c).map(Table::phys) {
                        if !needed(span, span + PAGE_2M) {
                            l2.set(c, 0);
                            unlinked.push(l3);
                        }
                    }
                }
                if l2.0.iter().all(|&e| e == 0) && !needed(base_b, base_b + (1 << 30)) {
                    let table = l2.phys();
                    l1.set(b, 0);
                    unlinked.push(table);
                }
            }
            if l1.0.iter().all(|&e| e == 0) && !needed(base_a, base_a + (1 << 39)) {
                let table = l1.phys();
                root.set(a, 0);
                unlinked.push(table);
            }
        }
        published();
        unlinked
    }

    /// Every CPU has dropped what it cached of `[start, end)` before this
    /// returns, and of the tables `unlinked` took out: a walk cache entry for
    /// another address under one of them is dropped only by the whole ASID.
    /// Answers whether it reached another CPU by interrupt: never, since
    /// the invalidations broadcast in hardware.
    pub fn shoot_down(&self, start: u64, end: u64, unlinked: &Unlinked) -> bool {
        const BY_PAGE: u64 = 64;
        let asid = self.asid.value();
        if !unlinked.is_empty() || (end - start) / PAGE_4K > BY_PAGE {
            tlb::asid(asid);
        } else {
            (start..end).step_by(PAGE_4K as usize).for_each(|va| tlb::page(asid, va));
        }
        false
    }
}

/// Frees every table under the root, and the root; the frames its leaves map
/// were freed by their owners first. No CPU walks a space that drops: its
/// last thread has left it, and its ASID is dropped everywhere before it is
/// issued again.
impl Drop for UserTables {
    fn drop(&mut self) {
        assert!(matches!(self.asid, Asid::User(_)), "paging: the kernel's address space dropped");
        let all = self.unlink(0, toyos_userbound::USER_TOP, |_, _| false);
        all.free(&self.job);
        free_table(&self.job, self.tables.root);
    }
}

/// The kernel's own tables, `TTBR1_EL1`'s.
static HIGH: Lock<Option<Tables>> = Lock::new(None);

fn high() -> LockGuard<'static, Option<Tables>> {
    HIGH.lock()
}

/// The kernel space's root, an empty user half, cached for lock-free access
/// from panic and crash paths.
static KERNEL_TTBR0: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// The kernel's own root, which every CPU's `TTBR1_EL1` holds from [`join`] on.
static KERNEL_TTBR1: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

pub fn kernel_root() -> Root {
    Root(KERNEL_TTBR0.load(core::sync::atomic::Ordering::Relaxed))
}

/// Leave whatever user half is current for the kernel's empty one.
pub fn activate_kernel() {
    // SAFETY: the kernel space's root is an empty table that lives forever.
    unsafe { kernel_root().activate() };
}

/// What [`map_mmio`] maps at: a 4 KiB page.
pub const MMIO_GRAIN: u64 = PAGE_4K;

/// Map a device's registers, or the scanout, into the direct map: 4 KiB pages
/// exactly over `[phys, phys + size)`, a block where a whole 2 MiB page is
/// asked for and free. No invalidation is owed, since only an invalid entry is
/// ever written.
pub fn map_mmio(phys: u64, size: u64, policy: MmioPolicy) -> crate::mm::Mmio {
    high()
        .as_mut()
        .expect("map_mmio: before the kernel's tables exist")
        .map_direct(phys, size, kernel_leaf(policy.cache(), Prot::ReadWrite));
    let installed = direct_map_policy(phys).expect("map_mmio: the window was just mapped");
    assert!(installed == policy.cache(), "map_mmio: {phys:#x} installed {installed:?}");
    crate::log!("mmio: {phys:#x}+{size:#x} {installed:?}");
    crate::mm::Mmio::new(DirectMap::from_phys(phys), size)
}

/// Build the kernel's tables — every page of memory firmware's map names, in
/// 4 KiB pages where a 2 MiB one holds more than one of `image`'s
/// protections, and the console
/// UART and the scanout the boot's own records reach — switch `TTBR1_EL1` to
/// them and `TTBR0_EL1` to the kernel's empty user half, and answer where the
/// direct map ends.
pub(crate) fn init(
    memory_map: &[MemoryMapEntry],
    scanout: Option<(u64, u64)>,
    image: &crate::mm::Image,
) -> toyos_bootmap::DirectMapEnd {
    let extent = direct_map_end(memory_map).unwrap_or_else(|refusal| panic!("paging: firmware's memory map: {refusal}"));
    let end = extent.get();
    let memory = |phys: u64| kernel_leaf(CachePolicy::Normal, image.prot(phys).unwrap_or(Prot::ReadWrite));
    let mut tables = Tables::new();
    let (mut blocks, mut pages) = (0u64, 0u64);
    let mut at = 0;
    while at < end {
        let va = PHYS_OFFSET + at;
        match coverage(memory_map, at) {
            Coverage::Nothing => {}
            Coverage::Whole if image.block(at).is_some() => {
                tables.directory(va).set(index(va, 2), at | memory(at));
                blocks += 1;
            }
            held => {
                let table = Table::new();
                for j in (0..512).filter(|&j| held.holds(j)) {
                    let page = at + j * PAGE_4K;
                    table.0[j as usize] = page | memory(page) | TABLE;
                    pages += 1;
                }
                let table = table.phys();
                tables.directory(va).set(index(va, 2), table | TABLE | VALID);
            }
        }
        at += PAGE_2M;
    }
    if let Some(uart) = super::console_uart::frame() {
        tables.map_direct(uart, PAGE_4K, kernel_leaf(CachePolicy::Uncacheable, Prot::ReadWrite));
    }
    if let Some((scanout, size)) = scanout {
        tables.map_direct(scanout, size, kernel_leaf(CachePolicy::WriteCombining, Prot::ReadWrite));
    }

    KERNEL_TTBR1.store(tables.root, core::sync::atomic::Ordering::Release);
    *high() = Some(tables);
    // ASID 0's, an empty table that lives forever.
    KERNEL_TTBR0.store(Table::new().phys(), core::sync::atomic::Ordering::Release);

    join();
    crate::log!("paging: the direct map holds memory below {end:#x} in {blocks} 2 MiB blocks and {pages} 4 KiB pages");
    extent
}

/// Put this CPU on the kernel's tables — `TTBR1_EL1` the kernel's root,
/// `TTBR0_EL1` the kernel space's empty user half — from the loader's or from
/// [`bringup_root`].
///
/// The loader's map and the kernel's need not agree on a leaf's size or
/// permissions for the same address: the loader maps 2 MiB blocks where the
/// kernel splits its image into pages, and leaves rodata writable. A change of
/// block size needs break-before-make (Arm ARM DDI 0487 M.d D8.17.1), or the
/// TLB may hold both translations at once and take a TLB conflict abort. So the
/// old `TTBR1_EL1` is replaced by an empty root and every translation dropped
/// with `TLBI VMALLE1` before the kernel's root is installed, while this CPU
/// runs from its image's identity alias under the old `TTBR0_EL1`, where both
/// the loader's map and `bringup_root` map the kernel's text executable. The
/// new `TTBR0_EL1` maps nothing, so switching it only removes translations.
/// Interrupts stay masked across it: the vectors are at `PHYS_OFFSET`.
pub(crate) fn join() {
    let ttbr1 = KERNEL_TTBR1.load(core::sync::atomic::Ordering::Acquire);
    let ttbr0 = KERNEL_TTBR0.load(core::sync::atomic::Ordering::Acquire);
    // SAFETY: both roots are published by `init` and live forever, and the
    // kernel space's root is an empty table, so it is the break's `TTBR1_EL1`
    // too. Between the two branches nothing is fetched or accessed but this
    // block's instructions, through the identity alias the old `TTBR0_EL1`
    // maps; the image runs at `PHYS_OFFSET` plus its physical address, so that
    // alias is the link address less `PHYS_OFFSET`.
    unsafe {
        core::arch::asm!(
            "mrs {daif}, daif",
            "msr daifset, #0xf",
            "dsb ishst",
            "adr {at}, 1f",
            "sub {at}, {at}, {offset}",
            "br {at}",
            "1:",
            "msr ttbr1_el1, {empty}",
            "isb",
            "tlbi vmalle1",
            "dsb nsh",
            "isb",
            "msr ttbr1_el1, {ttbr1}",
            "isb",
            "adr {at}, 2f",
            "add {at}, {at}, {offset}",
            "br {at}",
            "2:",
            "msr ttbr0_el1, {ttbr0}",
            "isb",
            "tlbi vmalle1",
            "dsb nsh",
            "isb",
            "msr daif, {daif}",
            daif = out(reg) _,
            at = out(reg) _,
            offset = in(reg) PHYS_OFFSET,
            empty = in(reg) ttbr0 & ADDR,
            ttbr1 = in(reg) ttbr1,
            ttbr0 = in(reg) ttbr0,
            options(nostack, preserves_flags),
        );
    }
}

/// What [`join`] wrote, read back: a CPU on any other roots is refused.
pub(crate) fn check_joined() {
    let (ttbr1, ttbr0): (u64, u64);
    // SAFETY: reads two translation registers; touches no memory.
    unsafe {
        core::arch::asm!(
            "mrs {ttbr1}, ttbr1_el1",
            "mrs {ttbr0}, ttbr0_el1",
            ttbr1 = out(reg) ttbr1,
            ttbr0 = out(reg) ttbr0,
            options(nomem, nostack, preserves_flags),
        );
    }
    let root1 = KERNEL_TTBR1.load(core::sync::atomic::Ordering::Acquire);
    let root0 = KERNEL_TTBR0.load(core::sync::atomic::Ordering::Acquire);
    assert!(
        (ttbr1, ttbr0) == (root1, root0),
        "paging: this CPU holds TTBR1_EL1={ttbr1:#x} TTBR0_EL1={ttbr0:#x}, and the kernel's roots are {root1:#x} and {root0:#x}"
    );
}

/// A root for a CPU to turn its MMU on under, running at physical addresses:
/// the kernel root's direct map, and the same entries again at identity below
/// it, so one root serves both `TTBR`s as the loader's does. Its tables below
/// the root are the kernel's own. Leaked: a CPU that answered too late to be
/// counted may walk it still.
pub fn bringup_root() -> u64 {
    let high = high();
    let kernel = high.as_ref().expect("bringup_root: before the kernel's tables exist").root();
    let root = Table::new();
    let from = index(PHYS_OFFSET, 0);
    for i in from..512 {
        root.0[i - from] = kernel.0[i];
        root.0[i] = kernel.0[i];
    }
    published();
    root.phys()
}

/// Nothing to seal: every space runs under the one `TTBR1_EL1`, so no space
/// holds a copy of the kernel's root slots to fall behind.
pub(crate) fn seal_kernel_half() {}

/// [`kernel::wx::audit`] of the kernel's tables, `TTBR1_EL1`'s on every CPU;
/// the kernel space's `TTBR0_EL1` half maps nothing.
pub fn audit_wx(text: core::ops::Range<u64>) -> kernel::wx::Verdict {
    let high = high();
    let tables = high.as_ref().expect("audit_wx: before the kernel's tables exist");
    // SAFETY: every table the kernel's root names is one this file linked, and
    // the lock held keeps them in place while they are read.
    let table = |phys: u64| unsafe { &Table::at(phys).0 };
    kernel::wx::audit::<kernel::wx::Aarch64>(&tables.root().0, text, &table)
}

/// `kernel-wx-leaf`: the direct map's leaf over `phys` made executable at
/// EL1, so [`audit_wx`] has a writable executable leaf to find. Answers its
/// address. Nothing runs from it, so no invalidation is owed.
#[cfg(feature = "boot-actuators")]
pub fn stage_writable_executable(phys: u64) -> u64 {
    let va = PHYS_OFFSET + phys;
    let mut high = high();
    let tables = high.as_mut().expect("kernel-wx-leaf: before the kernel's tables exist");
    let i = index(va, 2);
    let entry = tables.directory(va).0[i];
    assert!(entry & VALID != 0, "kernel-wx-leaf: {phys:#x} is not in the direct map");
    let (table, slot, at) = if entry & TABLE == 0 {
        (tables.directory(va), i, va & !(PAGE_2M - 1))
    } else {
        // SAFETY: a table this root linked, reached under the lock.
        (unsafe { Table::at_mut(entry & ADDR) }, index(va, 3), va & !(PAGE_4K - 1))
    };
    let leaf = table.0[slot];
    assert!(leaf & VALID != 0 && leaf & AP_READ_ONLY == 0, "kernel-wx-leaf: {phys:#x} is not under a writable leaf");
    table.set(slot, leaf & !PXN);
    published();
    at
}

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
        if !through_direct_map(entry) {
            return None;
        }
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

/// x86-64's stages a global kernel leaf going stale, and this architecture's
/// kernel leaves are not judged by it: refused.
#[cfg(feature = "test-actuators")]
pub fn debug_global_leaf(_how: u64) -> u64 {
    toyos_abi::syscall::SyscallError::NotSupported.to_u64()
}
