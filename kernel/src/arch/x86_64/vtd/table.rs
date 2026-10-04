//! Tables the unit walks: a root table, a context table per bus, and
//! second-level page tables for the kernel's one domain.
//!
//! `ECAP.PT` is clear on every unit this boots, so this module never writes
//! a passthrough context entry; `ECAP.C` is clear on QEMU, so every write
//! here flushes its cache line before returning.

use alloc::vec::Vec;

use crate::iommu::{AddressWidth, IommuError, Iova, StreamId};
use crate::mm::pmm::{self, PhysPage};
use crate::mm::{DirectMap, Mmio, PAGE_2M};

/// 4 KiB per table: 256 16-byte entries (root/context) or 512 8-byte entries (second-level).
const TABLE_BYTES: usize = 4096;

/// Conservative `clflush` line size: stepping by it can over-flush, never under-flush.
const LINE_BYTES: usize = 64;

const SL_READ: u64 = 1 << 0;
const SL_WRITE: u64 = 1 << 1;
/// Bit 7 at a page-directory level: a 2 MiB leaf rather than a pointer to the next level — the kernel's only leaf size.
const SL_LARGE: u64 = 1 << 7;

/// Root, context and second-level entries share this pointer field, bounded by x86-64's 52-bit physical ceiling.
const ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;

const PRESENT: u64 = 1 << 0;

/// The kernel's one domain; not 0, which an all-zero context entry also names — reusing it would blur a fault record and a domain-selective invalidation.
pub const KERNEL_DOMAIN: u16 = 1;

/// Both, in every domain: the coarsest split a driver's pools offer is finer
/// than a 2 MiB leaf, so nothing here can be narrowed to one of them.
const LEAF_PERM: u64 = SL_READ | SL_WRITE;

/// 4 KiB remapping tables carved out of 2 MiB PMM pages; never freed, since releasing one would need an invalidate-before-release ordering this allocator's types don't express.
pub struct Tables {
    pages: Vec<PhysPage>,
    /// Bytes taken from the newest page; starts full so the first call allocates one.
    used: usize,
}

impl Tables {
    pub const fn new() -> Self {
        Self { pages: Vec::new(), used: PAGE_2M as usize }
    }

    /// Returns one zeroed 4 KiB table, usable as a root, context, second-level, or invalidation-queue table.
    pub fn alloc(&mut self) -> Table {
        if self.used + TABLE_BYTES > PAGE_2M as usize {
            let page = pmm::alloc_page()
                .expect("iommu: no physical memory for a remapping table");
            self.pages.push(page);
            self.used = 0;
        }
        let base = self.pages.last().expect("iommu: table page just pushed").direct_map().phys();
        let table = Table { phys: base + self.used as u64 };
        self.used += TABLE_BYTES;
        // Zeroed through the direct map, but still only in cache the unit does not snoop.
        table.flush_all();
        table
    }
}

/// One 4 KiB remapping table, named by the physical address the unit sees it as.
#[derive(Clone, Copy)]
pub struct Table {
    phys: u64,
}

impl Table {
    pub fn phys(self) -> u64 {
        self.phys
    }

    /// The table's whole 4 KiB, bounding every offset and length checked against it.
    fn window(self) -> Mmio {
        // SAFETY: `self.phys` is a table `Tables::alloc` never frees, or an
        // entry this module wrote; the direct map covers it for the machine's
        // life, and volatile access matches the unit's non-snooped walks.
        unsafe { Mmio::over_phys(DirectMap::from_phys(self.phys), TABLE_BYTES as u64) }
    }

    /// The 8 bytes at slot `index`, bounded before the `clflush` that has no length of its own.
    fn slot(self, index: usize) -> Mmio {
        self.window().subregion(index as u64 * 8, 8)
    }

    /// Write is never callable without its flush; the split would be the ECAP.C=0 bug itself.
    fn write(self, index: usize, value: u64) {
        let slot = self.slot(index);
        slot.write_u64(0, value);
        flush(slot.addr() as usize);
    }

    fn read(self, index: usize) -> u64 {
        self.slot(index).read_u64(0)
    }

    /// Writes a 16-byte entry low half last, since the low half is what makes it live.
    pub fn write_pair(self, index: usize, lo: u64, hi: u64) {
        self.write(index * 2 + 1, hi);
        self.write(index * 2, lo);
    }

    /// Zeroes a 16-byte entry low half first: the low half holds `P`, and an
    /// entry whose high half went first would for a moment be present over a
    /// field it no longer means.
    pub fn clear_pair(self, index: usize) {
        self.write(index * 2, 0);
        self.write(index * 2 + 1, 0);
    }

    /// The 16-byte entry at `index`, back out of memory: `write` flushed the line, so this refetches.
    pub fn read_pair(self, index: usize) -> (u64, u64) {
        (self.read(index * 2), self.read(index * 2 + 1))
    }

    /// Swaps the present 16-byte entry `old` at `index` for `new` in one
    /// `lock cmpxchg16b`: the unit may fetch it at any moment and must read
    /// the one or the other, never a half of each (§6.2.2.1).
    fn replace_pair(self, index: usize, old: (u64, u64), new: (u64, u64)) {
        let entry = self.window().subregion(index as u64 * 16, 16);
        let (found_lo, found_hi): (u64, u64);
        // SAFETY: `entry` is 16 bytes of a table `Tables::alloc` never frees,
        // 16-byte aligned since a table is 4 KiB-aligned; `rbx`, which LLVM
        // reserves, holds `new`'s low half only between the two moves.
        // Every operand names its register: LLVM may give `rbx` to a `reg`
        // operand, and the `xchg` would then swap the address out of it.
        unsafe {
            core::arch::asm!(
                "xchg r8, rbx",
                "lock cmpxchg16b xmmword ptr [rsi]",
                "mov rbx, r8",
                in("rsi") entry.addr(),
                inout("r8") new.0 => _,
                in("rcx") new.1,
                inout("rax") old.0 => found_lo,
                inout("rdx") old.1 => found_hi,
                options(nostack),
            );
        }
        let found = (found_lo, found_hi);
        // `found` is `old` exactly when the swap happened; nothing else
        // writes an entry under `UNITS`, so anything else is a kernel bug.
        assert!(
            found == old,
            "iommu: entry {index} of table {:#x} held {:#x}:{:#x}, not the {:#x}:{:#x} read before \
             it was replaced",
            self.phys,
            found.1,
            found.0,
            old.1,
            old.0
        );
        flush(entry.addr() as usize);
    }

    fn flush_all(self) {
        let base = self.window().addr() as usize;
        for offset in (0..TABLE_BYTES).step_by(LINE_BYTES) {
            flush(base + offset);
        }
    }

    /// Writes a 32-bit field the unit will read, such as an invalidation queue's status.
    pub fn write_u32(self, byte_offset: usize, value: u32) {
        let field = self.window().subregion(byte_offset as u64, 4);
        field.write_u32(0, value);
        flush(field.addr() as usize);
    }

    /// Reads a 32-bit field the unit wrote, flushing the line first rather than trusting the cache.
    pub fn read_device_u32(self, byte_offset: usize) -> u32 {
        let field = self.window().subregion(byte_offset as u64, 4);
        flush(field.addr() as usize);
        field.read_u32(0)
    }
}

/// Flushes one line and fences it visible before the MMIO write that arms the unit; `clflush` not `clflushopt`, absent on QEMU's `qemu64`.
fn flush(addr: usize) {
    // SAFETY: `clflush` has no safe spelling and takes no length; every caller
    // bounds `addr` against the table's checked 4 KiB before calling, and
    // neither instruction touches memory the compiler can see.
    unsafe {
        core::arch::asm!(
            "clflush [{addr}]",
            "mfence",
            addr = in(reg) addr,
            options(nostack, preserves_flags),
        );
    }
}

/// Second-level table depth for `width`; the context entry's `AW` field is this minus two.
const fn levels(width: AddressWidth) -> u8 {
    match width {
        AddressWidth::Bits39 => 3,
        AddressWidth::Bits48 => 4,
    }
}

/// Builds the identity domain's second-level tables over `[0, top)`, returning the root and leaf count.
/// Not isolation: every address here is one a device could already reach with no unit on the machine.
/// `top` comes from the memory manager, not the firmware map, whose buffer is ordinary free RAM by the time this runs.
pub fn identity_domain(tables: &mut Tables, width: AddressWidth, top: u64) -> (Table, u64) {
    let levels = levels(width);
    let root = tables.alloc();
    let mut frames = 0u64;
    let mut phys = 0u64;
    while phys < top {
        map_2m(tables, root, levels, Iova::identity(phys), phys, LEAF_PERM);
        phys += PAGE_2M;
        frames += 1;
    }
    (root, frames)
}

/// One domain's second-level tables, its id, and how far up its addresses have been handed out.
#[derive(Clone, Copy)]
pub struct Domain {
    root: Table,
    id: u16,
    width: AddressWidth,
    /// Where its addresses start: [`Domain::first_address`] of what its unit
    /// translates, which is not [`AddressWidth::bits`] — see
    /// [`Domain::translatable_bits`].
    floor: u64,
    /// Where they end: [`ceiling`].
    ceiling: u64,
    next: u64,
}

/// Where a domain's addresses end: under what its unit translates, and under
/// the first of `reserved` that reaches above `floor` — a root bridge's window,
/// which a bridge may route peer-to-peer before the unit sees the request
/// (PCIe Base §2.4), or a region firmware reserved (VT-d §3.16). At or below
/// `floor` where one of them covers it.
const fn ceiling(translatable: u8, floor: u64, reserved: &[(u64, u64)]) -> u64 {
    let mut ceiling = 1u64 << translatable;
    let mut i = 0;
    while i < reserved.len() {
        let (start, end) = reserved[i];
        if end > floor && start < ceiling {
            ceiling = start;
        }
        i += 1;
    }
    ceiling
}

impl Domain {
    /// The bits of device address a unit will translate: the lesser of the page
    /// tables' depth and what the hardware accepts at all.
    ///
    /// **`SAGAW` and `MGAW` are two different limits and the smaller binds.**
    /// VT-d Rev. 4.0 D51397-015, 11.4.2 Capability Register: `SAGAW` 12:8 is the
    /// set of page-table depths a unit supports, `MGAW` 21:16 (encoded one less
    /// than it is) is the maximum DMA virtual addressability it has. A unit
    /// reporting a 48-bit `SAGAW` and a 39-bit `MGAW` walks four levels and
    /// still faults `address-beyond-mgaw` on anything from `1 << 39` up, so a
    /// window placed by table depth alone is unreachable on it.
    const fn translatable_bits(width: AddressWidth, mgaw: u8) -> u8 {
        if mgaw < width.bits() { mgaw } else { width.bits() }
    }

    /// A quarter of the way up what this domain can translate.
    const fn first_address(translatable: u8) -> u64 {
        1 << (translatable - 2)
    }

    /// A domain with `room` bytes of addresses between its floor and its
    /// [`ceiling`], or the reason it has not.
    pub fn new(
        tables: &mut Tables,
        id: u16,
        width: AddressWidth,
        mgaw: u8,
        reserved: &[(u64, u64)],
        room: u64,
    ) -> Result<Self, IommuError> {
        let translatable = Self::translatable_bits(width, mgaw);
        let floor = Self::first_address(translatable);
        // Above memory, so a descriptor still carrying one of these names
        // nothing this domain maps and faults rather than landing on a page.
        let top = crate::mm::pmm::top();
        if floor <= top {
            return Err(IommuError::WindowBelowMemory { translatable, floor, top });
        }
        let ceiling = ceiling(translatable, floor, reserved);
        // At least one leaf, whatever was asked: a domain with none is no domain.
        if ceiling.saturating_sub(floor) < room.next_multiple_of(PAGE_2M).max(PAGE_2M) {
            return Err(IommuError::NoRoom { floor, ceiling, room });
        }
        Ok(Self { root: tables.alloc(), id, width, floor, ceiling, next: floor })
    }

    pub fn root(&self) -> Table {
        self.root
    }

    pub fn id(&self) -> u16 {
        self.id
    }

    pub const fn floor(&self) -> u64 {
        self.floor
    }

    pub const fn ceiling(&self) -> u64 {
        self.ceiling
    }

    /// Reserve room for `bytes`, rounded up to whole leaves. An address is
    /// never handed out twice, unmapped or not: a device holding a stale one
    /// would reach whatever took its place.
    pub fn reserve(&mut self, bytes: u64) -> Option<Iova> {
        let span = bytes.next_multiple_of(PAGE_2M);
        let end = self.next.checked_add(span)?;
        if end > self.ceiling() {
            return None;
        }
        let at = Iova::translated(self.next);
        self.next = end;
        Some(at)
    }

    /// Whether `bytes` at `at` is room [`Self::reserve`] already handed out:
    /// the only room a mapping may be placed in by address.
    ///
    /// `at` must itself be a leaf `reserve` could have returned, not merely
    /// inside a handed-out span: [`map_2m`]'s index floors to the enclosing
    /// leaf, so an unaligned `at` this let through would silently place a
    /// mapping short of, or overlapping, where the caller named.
    pub const fn handed_out(&self, at: Iova, bytes: u64) -> bool {
        if !at.raw().is_multiple_of(PAGE_2M) {
            return false;
        }
        let span = bytes.next_multiple_of(PAGE_2M);
        if at.raw() < self.floor() {
            return false;
        }
        match at.raw().checked_add(span) {
            Some(end) => end <= self.next,
            None => false,
        }
    }
}

/// [`Domain::handed_out`] is a pure predicate on a `Copy` struct: checked here
/// at compile time rather than under a test harness this no-`std` binary has
/// none of.
const _: () = {
    const ROOT: Table = Table { phys: 0 };
    // `translatable = 48` puts `floor()` (a quarter of `1 << 48`) at `1 << 46`,
    // itself far past `PAGE_2M`-aligned.
    const FLOOR: u64 = Domain::first_address(48);
    const ONE_LEAF: Domain = Domain {
        root: ROOT,
        id: KERNEL_DOMAIN + 1,
        width: AddressWidth::Bits48,
        floor: FLOOR,
        ceiling: 1 << 48,
        next: FLOOR + PAGE_2M,
    };
    const TWO_LEAVES: Domain = Domain { next: FLOOR + 2 * PAGE_2M, ..ONE_LEAF };

    // Exactly what one `reserve(PAGE_2M)` handed out.
    assert!(ONE_LEAF.handed_out(Iova::translated(FLOOR), PAGE_2M));
    // Short of the floor: nothing this domain has ever reserved.
    assert!(!ONE_LEAF.handed_out(Iova::translated(FLOOR - PAGE_2M), PAGE_2M));
    // Past what has been reserved so far.
    assert!(!ONE_LEAF.handed_out(Iova::translated(FLOOR + PAGE_2M), PAGE_2M));
    // A byte count is rounded up to the leaf it needs, not truncated to fit.
    assert!(!ONE_LEAF.handed_out(Iova::translated(FLOOR), PAGE_2M + 1));
    // Two leaves handed out; the second is room in its own right.
    assert!(TWO_LEAVES.handed_out(Iova::translated(FLOOR + PAGE_2M), PAGE_2M));
    // Mid-span but off a leaf boundary: inside the handed-out range without
    // being an address `reserve` ever returned.
    assert!(!TWO_LEAVES.handed_out(Iova::translated(FLOOR + 1), PAGE_2M));
};

/// [`ceiling`] over the windows the T14's firmware declares, unsorted as it
/// declares them: a 39-bit unit's domain ends where the first window above its
/// floor begins, and a window reaching over the floor leaves it nothing.
const _: () = {
    const FLOOR: u64 = Domain::first_address(39);
    const T14: [(u64, u64); 7] = [
        (0xA200_0000, 0xBD00_0000),
        (0x40_0000_0000, 0x60_3DC0_0000),
        (0xA080_0000, 0xA200_0000),
        (0xBD00_0000, 0xC000_0000),
        (0xFF00_0000, 0xFFB8_0000),
        (0xFFD3_A070, 0x1_0000_0000),
        (0x60_3DC0_0000, 0x80_0000_0000),
    ];
    assert!(FLOOR == 0x20_0000_0000);
    assert!(ceiling(39, FLOOR, &T14) == 0x40_0000_0000);
    assert!(ceiling(39, FLOOR, &[]) == 1 << 39);
    assert!(ceiling(39, FLOOR, &[(0x10_0000_0000, FLOOR + 1)]) < FLOOR);
};

pub fn map(tables: &mut Tables, domain: &Domain, at: Iova, phys: u64, bytes: u64) {
    let levels = levels(domain.width);
    let mut offset = 0u64;
    while offset < bytes {
        map_2m(
            tables,
            domain.root,
            levels,
            Iova::translated(at.raw() + offset),
            phys + offset,
            LEAF_PERM,
        );
        offset += PAGE_2M;
    }
}

/// Clears the leaves covering `bytes` at `at`; the caller invalidates before the pages behind them are reused.
pub fn unmap(domain: &Domain, at: Iova, bytes: u64) -> Result<(), IommuError> {
    let levels = levels(domain.width);
    let mut offset = 0u64;
    while offset < bytes {
        let here = Iova::translated(at.raw() + offset);
        let (table, index) =
            leaf_of(domain.root, levels, here).ok_or(IommuError::NotMapped(here))?;
        if table.read(index) & (SL_READ | SL_WRITE) == 0 {
            return Err(IommuError::NotMapped(here));
        }
        table.write(index, 0);
        offset += PAGE_2M;
    }
    Ok(())
}

/// Walks to the page-directory holding `at`'s leaf, growing the tables on the way.
fn descend(tables: &mut Tables, root: Table, levels: u8, at: Iova) -> Table {
    let mut table = root;
    let mut level = levels;
    while level > 2 {
        let index = ((at.raw() >> (12 + 9 * (level as u64 - 1))) & 0x1FF) as usize;
        let entry = table.read(index);
        table = if entry & (SL_READ | SL_WRITE) != 0 {
            Table { phys: entry & ADDR_MASK }
        } else {
            let next = tables.alloc();
            // Grants both: the unit ANDs permissions down the walk, so narrowing here narrows everything below.
            table.write(index, next.phys | SL_READ | SL_WRITE);
            next
        };
        level -= 1;
    }
    table
}

/// The same walk without growing it: `None` where no table covers `at` at all.
fn leaf_of(root: Table, levels: u8, at: Iova) -> Option<(Table, usize)> {
    let mut table = root;
    let mut level = levels;
    while level > 2 {
        let index = ((at.raw() >> (12 + 9 * (level as u64 - 1))) & 0x1FF) as usize;
        let entry = table.read(index);
        if entry & (SL_READ | SL_WRITE) == 0 {
            return None;
        }
        table = Table { phys: entry & ADDR_MASK };
        level -= 1;
    }
    Some((table, ((at.raw() >> 21) & 0x1FF) as usize))
}

fn map_2m(tables: &mut Tables, root: Table, levels: u8, at: Iova, phys: u64, perm: u64) {
    let table = descend(tables, root, levels, at);
    let index = ((at.raw() >> 21) & 0x1FF) as usize;
    // A present leaf here is memory some holder still reaches: the caller
    // unmaps before it writes over one, so a leaf that is already live is a
    // dead holder's pages about to be silently repurposed under a live one.
    assert!(
        table.read(index) & (SL_READ | SL_WRITE) == 0,
        "iommu: {:#x} was still mapped when a new leaf was written there",
        at.raw()
    );
    table.write(index, (phys & !(PAGE_2M - 1)) | SL_LARGE | perm);
}

/// Translation type 00: untranslated requests route through the named second-level table.
const fn context_entry(domain: Table, id: u16, width: AddressWidth) -> (u64, u64) {
    (domain.phys | PRESENT, ((id as u64) << 8) | (levels(width) as u64 - 2))
}

/// What a unit may still hold cached for a context entry [`bind`] replaced:
/// its requester and the domain id it named, the pair §6.5.2.1 invalidates by.
#[must_use]
pub struct Displaced {
    requester: u16,
    domain: u16,
}

impl Displaced {
    pub fn requester(&self) -> u16 {
        self.requester
    }

    pub fn domain(&self) -> u16 {
        self.domain
    }
}

/// The entry that moves `requester`'s present entry `old` onto `domain`, and
/// what that leaves the unit holding: the id `old` named, never `domain`'s.
const fn rebind(old: (u64, u64), requester: u16, domain: &Domain) -> ((u64, u64), Displaced) {
    let new = context_entry(domain.root, domain.id, domain.width);
    (new, Displaced { requester, domain: (old.1 >> 8) as u16 })
}

/// Gives `stream` its first context entry, naming the identity domain.
pub fn bind_identity(
    tables: &mut Tables,
    root: Table,
    stream: StreamId,
    domain: Table,
    width: AddressWidth,
) {
    let bus = stream.bus() as usize;
    let entry = root.read(bus * 2);
    let context = if entry & PRESENT != 0 {
        Table { phys: entry & ADDR_MASK }
    } else {
        let table = tables.alloc();
        root.write_pair(bus, table.phys | PRESENT, 0);
        table
    };
    let (lo, hi) = context_entry(domain, KERNEL_DOMAIN, width);
    context.write_pair(stream.devfn() as usize, lo, hi);
}

/// Moves `stream`'s present context entry in one unit's root table onto a
/// domain of its own, and answers what that unit may still hold for the old one.
pub fn bind(root: Table, stream: StreamId, domain: &Domain) -> Displaced {
    let entry = root.read(stream.bus() as usize * 2);
    let context = Table { phys: entry & ADDR_MASK };
    let index = stream.devfn() as usize;
    let old = if entry & PRESENT != 0 { context.read_pair(index) } else { (0, 0) };
    // `bind_identity` gave every enumerated function one before `TE`.
    assert!(
        old.0 & PRESENT != 0,
        "iommu: {stream} has no context entry to move — it was not enumerated when its unit \
         was programmed"
    );
    let (new, displaced) = rebind(old, stream.requester(), domain);
    context.replace_pair(index, old, new);
    displaced
}

/// A function moved off the identity domain is invalidated under the identity
/// domain's id, the only one its cached entry can match (§6.5.2.1), and its new
/// entry names the domain it moved to.
const _: () = {
    const OWN: Domain = Domain {
        root: Table { phys: 0x5000 },
        id: u16::MAX,
        width: AddressWidth::Bits39,
        floor: 0,
        ceiling: 0,
        next: 0,
    };
    let identity = context_entry(Table { phys: 0x3000 }, KERNEL_DOMAIN, AddressWidth::Bits48);
    let (new, displaced) = rebind(identity, 0x00F8, &OWN);
    assert!(displaced.domain == KERNEL_DOMAIN && displaced.requester == 0x00F8);
    assert!(new.0 == 0x5000 | PRESENT && new.1 == (u16::MAX as u64) << 8 | 1);
};

/// A unit whose `MGAW` is narrower than its `SAGAW` gets its window under the
/// smaller of the two.
///
/// Asserted here rather than in a guest because no guest holds the shape: QEMU's
/// `intel-iommu` derives both fields from one `aw-bits` property, so its model
/// cannot report a `SAGAW` wider than its `MGAW` at all.
const _: () = {
    assert!(Domain::translatable_bits(AddressWidth::Bits48, 39) == 39);
    assert!(Domain::first_address(39) < 1 << 39);
    // What a window placed by the table depth alone would be, against the
    // ceiling such a unit reports.
    assert!(Domain::first_address(48) >= 1 << 39);
    // A unit whose two limits agree is unchanged by any of this.
    assert!(Domain::translatable_bits(AddressWidth::Bits48, 48) == 48);
    assert!(Domain::translatable_bits(AddressWidth::Bits39, 39) == 39);
    // And tables shallower than `MGAW` bind it the other way round.
    assert!(Domain::translatable_bits(AddressWidth::Bits39, 48) == 39);
};
