//! Tables the unit walks: a root table, a context table per bus, and
//! second-level page tables for the kernel's one domain.
//!
//! `ECAP.PT` is clear on every unit this boots, so this module never writes
//! a passthrough context entry; `ECAP.C` is clear on QEMU, so every write
//! here flushes its cache line before returning.

use alloc::vec::Vec;

use crate::iommu::{AddressWidth, IommuError, Iova, StreamId};
use crate::mm::pmm::{self, Category, PhysPage};
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
            let page = pmm::alloc_page(Category::Dma)
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

    /// The 16-byte entry at `index`, back out of memory: `write` flushed the line, so this refetches.
    pub fn read_pair(self, index: usize) -> (u64, u64) {
        (self.read(index * 2), self.read(index * 2 + 1))
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
fn levels(width: AddressWidth) -> u8 {
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
    /// Bits of device address this domain may hand out, which is not
    /// [`AddressWidth::bits`] — see [`Domain::translatable`].
    translatable: u8,
    next: u64,
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

    /// A quarter of the way up what this domain can translate — above any
    /// physical address these machines have, so a descriptor still carrying one
    /// names nothing this domain maps and faults rather than landing.
    const fn first_address(translatable: u8) -> u64 {
        1 << (translatable - 2)
    }

    pub fn new(
        tables: &mut Tables,
        id: u16,
        width: AddressWidth,
        mgaw: u8,
    ) -> Result<Self, IommuError> {
        let translatable = Self::translatable_bits(width, mgaw);
        let floor = Self::first_address(translatable);
        // The property `first_address` is chosen for, asserted rather than
        // assumed: a machine with enough memory to reach the window would have
        // stale descriptors landing on real pages instead of faulting.
        let top = crate::mm::pmm::top();
        if floor <= top {
            return Err(IommuError::WindowBelowMemory { translatable, floor, top });
        }
        Ok(Self { root: tables.alloc(), id, width, translatable, next: floor })
    }

    pub fn root(&self) -> Table {
        self.root
    }

    pub fn id(&self) -> u16 {
        self.id
    }

    pub fn floor(&self) -> u64 {
        Self::first_address(self.translatable)
    }

    /// The bits of device address this domain hands out, for a refusal that
    /// names what ran out rather than the depth of the tables.
    pub fn translatable(&self) -> u8 {
        self.translatable
    }

    /// Reserve room for `bytes`, rounded up to whole leaves. An address is
    /// never handed out twice, unmapped or not: a device holding a stale one
    /// would reach whatever took its place.
    pub fn reserve(&mut self, bytes: u64) -> Option<Iova> {
        let span = bytes.next_multiple_of(PAGE_2M);
        let end = self.next.checked_add(span)?;
        // What this unit will translate, not what the tables can express: past
        // `MGAW` the hardware faults before the walk it has entries for.
        if end > 1u64 << self.translatable {
            return None;
        }
        let at = Iova::translated(self.next);
        self.next = end;
        Some(at)
    }
}

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
    table.write(index, (phys & !(PAGE_2M - 1)) | SL_LARGE | perm);
}

/// Gives `stream` a context entry naming the identity domain.
pub fn bind_identity(
    tables: &mut Tables,
    root: Table,
    stream: StreamId,
    domain: Table,
    width: AddressWidth,
) {
    write_context(tables, root, stream, domain, KERNEL_DOMAIN, width);
}

/// Moves `stream` onto a domain of its own, in one unit's root table.
pub fn bind(tables: &mut Tables, root: Table, stream: StreamId, domain: &Domain) {
    write_context(tables, root, stream, domain.root, domain.id, domain.width);
}

fn write_context(
    tables: &mut Tables,
    root: Table,
    stream: StreamId,
    domain: Table,
    id: u16,
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
    // Translation type 00: untranslated requests route through the named second-level table.
    let lo = domain.phys | PRESENT;
    let hi = ((id as u64) << 8) | (levels(width) as u64 - 2);
    context.write_pair(stream.devfn() as usize, lo, hi);
}

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
