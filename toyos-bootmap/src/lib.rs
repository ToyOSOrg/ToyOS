//! The bootloader's transient page tables, decided, and written into pages
//! the loader gives.
//!
//! Between the loader's switch to these tables and the kernel's `mm::init`
//! there is one mapping in the machine, and everything the kernel touches in
//! that window has to be in it — its own image, the boot parameter, and the
//! panel it reports a wedge on — and so is the loader's own image, wherever
//! firmware put it: on x86-64 the switch runs from it.
//!
//! Both architectures walk the same shape — a root (x86-64's PML4, AArch64's
//! L0), a table per 512 GiB (PDPT, L1), a table per GiB (page directory, L2)
//! of 2 MiB leaves — so one [`Plan`] serves both. What differs is how a leaf
//! is typed ([`Typing`]) and how an entry is encoded ([`x86_64`],
//! [`aarch64`]).
//!
//! A second-level table is position-independent, so one per 512 GiB the map
//! reaches serves both views: root slot [`ROOT_IDENTITY`] + r and
//! [`ROOT_HIGH_HALF`] + r name the same table, as both views already share
//! every directory. The map reaches as far as the high half can hold, and
//! refuses by name what lies past it.
//!
//! Pure: the scanout, the loader's image and, where the architecture types
//! memory by firmware's map, the write-back ranges in; a [`Plan`] out, which
//! [`Plan::write`] lays into a pool of [`Table`]s the loader allocates.
//!
//! What the kernel takes from firmware's map when it builds its own tables is
//! here too, because it may never map less than this map did: which types the
//! pmm hands out ([`is_usable_type`]), and how far the direct map reaches
//! ([`x86_64::direct_map_end`], [`DirectMapEnd`]). The types are `EFI_MEMORY_TYPE`'s,
//! and what an OS may do with each after `ExitBootServices` is the UEFI
//! specification's table under `EFI_BOOT_SERVICES.AllocatePages()` (§7.2).
//! That table puts no bound on where a range the OS does not use may sit, and
//! UEFI does not order the map.

#![no_std]
#![forbid(unsafe_code)]

use core::fmt;
use core::sync::atomic::{AtomicU64, Ordering};

pub mod aarch64;
pub mod x86_64;

/// The page every entry in this map describes.
pub const PAGE_2M: u64 = 2 * 1024 * 1024;

const GIB: u64 = 1 << 30;

/// One second-level table reaches 512 GiB.
const GIB_PER_PDPT: u64 = 512;

/// Root slot 0: physical memory at its own address, which the switch to these
/// tables itself runs from.
pub const ROOT_IDENTITY: usize = 0;

/// Root slot 256: the same memory at `PHYS_OFFSET`, whose bits 39..48 are 256
/// and which is where the kernel runs from.
pub const ROOT_HIGH_HALF: usize = 256;

/// How much physical memory the map covers, at identity and at `PHYS_OFFSET`
/// alike. Everything the entry jump needs, not everything `KernelArgs` names.
pub const BOOT_MAP_BYTES: u64 = 4 * GIB;

/// The physical addresses a direct map at root slot [`ROOT_HIGH_HALF`] can
/// hold: every slot from there to the root's last.
pub const DIRECT_MAP_WINDOW: u64 = (512 - ROOT_HIGH_HALF as u64) * GIB_PER_PDPT * GIB;

/// One past the kernel direct map's last byte.
///
/// ```compile_fail,E0603
/// let _ = toyos_bootmap::DirectMapEnd(1 << 52);
/// ```
#[derive(Clone, Copy)]
pub struct DirectMapEnd(u64);

impl DirectMapEnd {
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Where a [`DirectMapEnd`] is kept between the map that decided it and the
/// readers that ask; it holds no other number.
pub struct DirectMapEndCell(AtomicU64);

impl DirectMapEndCell {
    /// Holding [`BOOT_MAP_BYTES`], until the kernel's own map decides wider.
    pub const fn boot() -> Self {
        Self(AtomicU64::new(BOOT_MAP_BYTES))
    }

    pub fn set(&self, end: DirectMapEnd) {
        self.0.store(end.0, Ordering::Release);
    }

    pub fn get(&self) -> DirectMapEnd {
        DirectMapEnd(self.0.load(Ordering::Acquire))
    }
}

const EFI_LOADER_CODE: u32 = 1;
pub const EFI_LOADER_DATA: u32 = 2;
const EFI_BOOT_SERVICES_CODE: u32 = 3;
const EFI_BOOT_SERVICES_DATA: u32 = 4;
const EFI_CONVENTIONAL_MEMORY: u32 = 7;
const EFI_ACPI_RECLAIM_MEMORY: u32 = 9;
const EFI_ACPI_MEMORY_NVS: u32 = 10;

/// Whether a UEFI memory type becomes free RAM the pmm hands out.
pub const fn is_usable_type(uefi_type: u32) -> bool {
    matches!(
        uefi_type,
        EFI_LOADER_CODE
            | EFI_LOADER_DATA
            | EFI_BOOT_SERVICES_CODE
            | EFI_BOOT_SERVICES_DATA
            | EFI_CONVENTIONAL_MEMORY
    )
}

/// Whether the kernel reads a range of this type as memory: what the pmm hands
/// out, and the two types ACPI's tables live in. Any other type, one this list
/// does not know included, is not.
const fn is_read_as_memory(uefi_type: u32) -> bool {
    is_usable_type(uefi_type) || matches!(uefi_type, EFI_ACPI_RECLAIM_MEMORY | EFI_ACPI_MEMORY_NVS)
}

/// One page directory per GiB of [`BOOT_MAP_BYTES`].
const LOW_DIRECTORIES: usize = (BOOT_MAP_BYTES / GIB) as usize;

/// What a scanout adds: it lies inside one GiB or straddles two, and a wider
/// one is [`Refusal::Directories`] rather than a silent overrun.
const SCANOUT_DIRECTORIES: usize = 2;

/// What the loader's own image adds, on the same terms.
const LOADER_DIRECTORIES: usize = 2;

/// Every page directory a [`Plan`] can name.
pub const MAX_DIRECTORIES: usize = LOW_DIRECTORIES + SCANOUT_DIRECTORIES + LOADER_DIRECTORIES;

/// Every second-level table a [`Plan`] can name: the low map's, and one per
/// directory past it, since a directory lies in exactly one 512 GiB.
pub const MAX_REGIONS: usize = 1 + SCANOUT_DIRECTORIES + LOADER_DIRECTORIES;

/// The small page a split 2 MiB page is mapped in.
pub const PAGE_4K: u64 = 4096;

/// Leaves in one table.
const PAGES_PER_TABLE: u64 = PAGE_2M / PAGE_4K;

/// The 2 MiB pages a scanout covers only in part: at most its first and its last.
const FINE_TABLES: usize = 2;

/// The pool a builder needs: a root, every second-level table, every
/// directory, and a table of 4 KiB leaves per split page.
pub const MAX_PAGES: usize = 1 + MAX_REGIONS + MAX_DIRECTORIES + FINE_TABLES;

/// One page of the map: 512 entries, on the page an entry names it by, since
/// an entry's low 12 bits are flags rather than address.
#[derive(Clone, Copy)]
#[repr(C, align(4096))]
pub struct Table(pub [u64; 512]);

impl Table {
    pub const EMPTY: Self = Self([0; 512]);
}

/// How an architecture encodes the three entries a [`Plan`] writes: one naming
/// the table below it, a 2 MiB leaf, and a 4 KiB leaf.
#[derive(Clone, Copy)]
pub struct Encoding {
    pub table: fn(u64) -> u64,
    pub block: fn(u64, Cache) -> u64,
    pub page: fn(u64, Cache) -> u64,
}

/// Why a machine's memory does not fit these tables.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
    /// A 2 MiB page cannot begin anywhere else, and rounding the base down
    /// would retype memory below it that firmware handed to somebody else.
    Unaligned(u64),
    /// The range's own end does not fit an address.
    Extent { base: u64, len: u64 },
    /// More directories than [`MAX_DIRECTORIES`].
    Directories(usize),
    /// A 2 MiB page of the low map that is write-back memory in part and not
    /// in the rest: either type is wrong for some of it.
    Mixed(u64),
    /// A range that ends here, past [`DIRECT_MAP_WINDOW`].
    PastWindow(u64),
    /// A range of firmware's map that begins or ends here, off the 4 KiB page
    /// UEFI describes memory in.
    OffPage(u64),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unaligned(base) => {
                write!(f, "{base:#x} is not on a {PAGE_2M:#x}-byte page")
            }
            Self::Extent { base, len } => {
                write!(f, "{base:#x}+{len:#x} runs past the end of the address space")
            }
            Self::Directories(needed) => {
                write!(f, "{needed} page directories are needed and {MAX_DIRECTORIES} may be named")
            }
            Self::Mixed(phys) => write!(
                f,
                "the 2 MiB page at {phys:#x} is part write-back memory and part not, so no one \
                 memory type is right for all of it"
            ),
            Self::PastWindow(end) => write!(
                f,
                "a range ends at {end:#x}, past the {DIRECT_MAP_WINDOW:#x} bytes a direct map can hold"
            ),
            Self::OffPage(at) => write!(f, "a range of firmware's map is bounded at {at:#x}, off a {PAGE_4K:#x}-byte page"),
        }
    }
}

/// What a 2 MiB entry is, which is what its memory type follows from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cache {
    /// Typed by the architecture's own range registers rather than by the
    /// entry: x86-64's MTRRs, beneath an entry that selects plain memory.
    Firmware,
    /// Write-back memory, every byte of it, by firmware's map.
    Memory,
    /// Not memory by firmware's map: registers, or nothing at all.
    Device,
    /// The scanout.
    Scanout,
}

/// How the pages that are not the scanout are typed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Typing<'a> {
    /// By the architecture's range registers: every such page is [`Cache::Firmware`].
    Firmware,
    /// By firmware's memory map: these ranges, `(base, length)`, are
    /// write-back memory; a page wholly outside them is a device's, and a page
    /// partly inside is [`Refusal::Mixed`].
    ByMap(&'a [(u64, u64)]),
}

impl Typing<'_> {
    /// The type of the `size`-byte page at `phys`.
    fn of(self, phys: u64, size: u64) -> Result<Cache, Refusal> {
        let Typing::ByMap(memory) = self else { return Ok(Cache::Firmware) };
        let end = phys + size;
        // Bytes of the page the ranges cover; firmware's ranges do not overlap.
        let covered: u64 = memory
            .iter()
            .map(|&(base, len)| base.saturating_add(len).min(end).saturating_sub(base.max(phys)))
            .sum();
        match covered {
            0 => Ok(Cache::Device),
            c if c == size => Ok(Cache::Memory),
            _ => Err(Refusal::Mixed(phys)),
        }
    }
}

/// Where a leaf sits.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Slot {
    /// A 2 MiB leaf: in which of [`Plan::directories`], by position, and at
    /// which index.
    Directory { directory: usize, index: usize },
    /// A 4 KiB leaf: in which of [`Plan::fine_tables`], by position, and at
    /// which index.
    Fine { table: usize, index: usize },
}

/// One leaf the map holds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Entry {
    pub phys: u64,
    pub slot: Slot,
    pub cache: Cache,
}

/// Where a machine's memory, its scanout and the loader itself go in the
/// loader's two views.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Plan<'a> {
    gibs: [u64; MAX_DIRECTORIES],
    directories: usize,
    /// The 512 GiB each second-level table covers, by number.
    spans: [u64; MAX_REGIONS],
    regions: usize,
    /// The 2 MiB pages split into 4 KiB leaves, by base.
    fine: [u64; FINE_TABLES],
    fines: usize,
    scanout: Option<(u64, u64)>,
    /// The loader's image in whole 2 MiB pages: `(first page, bytes)`.
    loader: (u64, u64),
    typing: Typing<'a>,
}

impl<'a> Plan<'a> {
    /// Lay out [`BOOT_MAP_BYTES`], `scanout` and `loader`, typed by `typing`,
    /// or say why they cannot be.
    ///
    /// `scanout` is firmware's framebuffer as firmware reports it. Under
    /// [`Typing::Firmware`] its base must be 2 MiB aligned and its length is
    /// rounded up to whole 2 MiB pages, because what the last one covers past
    /// the framebuffer is the same aperture firmware put it in. Under
    /// [`Typing::ByMap`] it need only be 4 KiB aligned: a 2 MiB page it covers
    /// in part is split into 4 KiB leaves, so the scanout's type reaches no
    /// byte beside it — a framebuffer firmware carved out of RAM sits between
    /// memory other owners hold.
    ///
    /// `loader` is the loader's own image as firmware loaded it, anywhere: the
    /// x86-64 loader's switch to these tables runs from it, so it is mapped at
    /// identity or the first fetch after the switch faults. It is plain memory,
    /// so its pages are rounded out both ways and typed as the rest of memory.
    ///
    /// A byte of either past [`DIRECT_MAP_WINDOW`] is [`Refusal::PastWindow`].
    pub fn new(scanout: Option<(u64, u64)>, loader: (u64, u64), typing: Typing<'a>) -> Result<Self, Refusal> {
        let mut plan = Self {
            gibs: [0; MAX_DIRECTORIES],
            directories: 0,
            spans: [0; MAX_REGIONS],
            regions: 0,
            fine: [0; FINE_TABLES],
            fines: 0,
            scanout: None,
            loader: whole_pages(loader.0, loader.1)?,
            typing,
        };
        let scanout = scanout.map(|(base, len)| plan.scanout_extent(base, len)).transpose()?;
        let spans = [
            scanout.map(|(base, end)| (base / PAGE_2M * PAGE_2M, end)),
            Some((plan.loader.0, plan.loader.0 + plan.loader.1)),
        ];
        // Counted whole before one is claimed, so a machine needing fourteen is
        // told fourteen rather than that one more than the budget was wanted.
        let required = LOW_DIRECTORIES + fresh_directories(spans);
        if required > MAX_DIRECTORIES {
            return Err(Refusal::Directories(required));
        }
        for gib in 0..BOOT_MAP_BYTES / GIB {
            plan.claim(gib);
        }
        for (first, end) in spans.into_iter().flatten() {
            for gib in first / GIB..=(end - 1) / GIB {
                plan.claim(gib);
            }
        }
        for at in 0..plan.directories {
            let region = plan.gibs[at] / GIB_PER_PDPT;
            if !plan.spans[..plan.regions].contains(&region) {
                plan.spans[plan.regions] = region;
                plan.regions += 1;
            }
        }
        if let Some((base, end)) = scanout {
            plan.split_scanout(base, end);
        }
        // Every leaf that is not the scanout typed once here, so `entries`
        // cannot meet a refusal.
        for page in 0..BOOT_MAP_BYTES / PAGE_2M {
            let phys = page * PAGE_2M;
            if !plan.is_split(phys) && !plan.in_scanout(phys, PAGE_2M) {
                typing.of(phys, PAGE_2M)?;
            }
        }
        for &base in plan.fine_tables() {
            for page in 0..PAGES_PER_TABLE {
                let phys = base + page * PAGE_4K;
                if !plan.in_scanout(phys, PAGE_4K) {
                    typing.of(phys, PAGE_4K)?;
                }
            }
        }
        for phys in plan.loader_pages() {
            typing.of(phys, PAGE_2M)?;
        }
        Ok(plan)
    }

    /// The scanout's extent as the map gives it, `(base, end)`, or why none can
    /// be given.
    fn scanout_extent(&self, base: u64, len: u64) -> Result<(u64, u64), Refusal> {
        let granule = match self.typing {
            Typing::Firmware => PAGE_2M,
            Typing::ByMap(_) => PAGE_4K,
        };
        if !base.is_multiple_of(granule) {
            return Err(Refusal::Unaligned(base));
        }
        let end = base.checked_add(len).ok_or(Refusal::Extent { base, len })?;
        let end = end.checked_next_multiple_of(granule).ok_or(Refusal::Extent { base, len })?;
        within_window(end)?;
        Ok((base, end))
    }

    /// Split the pages the scanout `base..end` covers in part, and record it.
    fn split_scanout(&mut self, base: u64, end: u64) {
        let first = base / PAGE_2M * PAGE_2M;
        let last = (end - 1) / PAGE_2M * PAGE_2M;
        // At most the first and the last page are covered in part.
        for page in [first, last] {
            let whole = base <= page && page + PAGE_2M <= end;
            if !whole && !self.is_split(page) {
                self.fine[self.fines] = page;
                self.fines += 1;
            }
        }
        self.scanout = Some((base, end - base));
    }

    /// Write this map into `pool`, a zeroed pool whose first byte is at
    /// physical address `at`, and return its root's: the root first, then
    /// [`Plan::regions`]' tables, the directories and the split pages' tables,
    /// each in its accessor's order.
    pub fn write(&self, encoding: Encoding, pool: &mut [Table; MAX_PAGES], at: u64) -> u64 {
        let first_directory = 1 + self.regions;
        let first_fine = first_directory + self.directories;
        let phys = |page: usize| at + page as u64 * PAGE_4K;
        for (r, &region) in self.regions().iter().enumerate() {
            let entry = (encoding.table)(phys(1 + r));
            pool[0].0[ROOT_IDENTITY + region as usize] = entry;
            pool[0].0[ROOT_HIGH_HALF + region as usize] = entry;
        }
        for (d, (region, index)) in self.directory_slots().enumerate() {
            pool[1 + region].0[index] = (encoding.table)(phys(first_directory + d));
        }
        for (f, (directory, index)) in self.fine_slots().enumerate() {
            pool[first_directory + directory].0[index] = (encoding.table)(phys(first_fine + f));
        }
        for entry in self.entries() {
            match entry.slot {
                Slot::Directory { directory, index } => {
                    pool[first_directory + directory].0[index] = (encoding.block)(entry.phys, entry.cache)
                }
                Slot::Fine { table, index } => {
                    pool[first_fine + table].0[index] = (encoding.page)(entry.phys, entry.cache)
                }
            }
        }
        phys(0)
    }

    /// The 512 GiB each second-level table covers, by number `r`, in the order
    /// a builder allocates them: each is named from root slots
    /// [`ROOT_IDENTITY`] + `r` and [`ROOT_HIGH_HALF`] + `r`.
    pub fn regions(&self) -> &[u64] {
        &self.spans[..self.regions]
    }

    /// The GiB each directory covers, in the order a builder allocates them.
    pub fn directories(&self) -> &[u64] {
        &self.gibs[..self.directories]
    }

    /// Where each of [`Plan::directories`] is named from: its second-level
    /// table, by position in [`Plan::regions`], and its index there.
    pub fn directory_slots(&self) -> impl Iterator<Item = (usize, usize)> + '_ {
        self.directories().iter().map(move |&gib| {
            let region = self
                .regions()
                .iter()
                .position(|r| *r == gib / GIB_PER_PDPT)
                .expect("every directory's 512 GiB was claimed");
            (region, (gib % GIB_PER_PDPT) as usize)
        })
    }

    /// The 2 MiB pages mapped by a table of 4 KiB leaves rather than one
    /// leaf, by base, in the order a builder allocates those tables.
    pub fn fine_tables(&self) -> &[u64] {
        &self.fine[..self.fines]
    }

    /// Where each of [`Plan::fine_tables`] is named from: its directory, by
    /// position, and its index there.
    pub fn fine_slots(&self) -> impl Iterator<Item = (usize, usize)> + '_ {
        self.fine_tables().iter().map(move |&base| self.directory_slot(base))
    }

    /// The scanout as this map covers it: firmware's base, and its length
    /// rounded up to whole leaves. `None` is a machine with no framebuffer.
    pub fn scanout(&self) -> Option<(u64, u64)> {
        self.scanout
    }

    /// The loader's image as this map covers it, in whole 2 MiB pages.
    pub fn loader(&self) -> (u64, u64) {
        self.loader
    }

    /// The pages the loader's image adds: those past the low map that neither
    /// the scanout nor a split page already holds.
    fn loader_pages(&self) -> impl Iterator<Item = u64> + '_ {
        let (first, len) = self.loader;
        (0..len / PAGE_2M)
            .map(move |page| first + page * PAGE_2M)
            .filter(move |phys| {
                *phys >= BOOT_MAP_BYTES && !self.is_split(*phys) && !self.in_scanout(*phys, PAGE_2M)
            })
    }

    /// Every leaf the map holds, low memory first and each place once: a
    /// scanout inside the low map retypes the leaves already there rather than
    /// adding a second one for them, and the loader adds only pages nothing
    /// else holds.
    pub fn entries(&self) -> impl Iterator<Item = Entry> + '_ {
        let low = (0..BOOT_MAP_BYTES / PAGE_2M)
            .map(|page| page * PAGE_2M)
            .filter(move |phys| !self.is_split(*phys))
            .map(move |phys| self.leaf(phys, self.cache_of(phys, PAGE_2M)));
        let scanout = self.scanout.into_iter().flat_map(move |(base, len)| {
            let first = base / PAGE_2M * PAGE_2M;
            (0..(base + len - first).div_ceil(PAGE_2M))
                .map(move |page| first + page * PAGE_2M)
                .filter(move |phys| *phys >= BOOT_MAP_BYTES && !self.is_split(*phys))
                .map(move |phys| self.leaf(phys, Cache::Scanout))
        });
        let fine = self.fine_tables().iter().enumerate().flat_map(move |(table, &base)| {
            (0..PAGES_PER_TABLE).map(move |index| {
                let phys = base + index * PAGE_4K;
                Entry {
                    phys,
                    slot: Slot::Fine { table, index: index as usize },
                    cache: self.cache_of(phys, PAGE_4K),
                }
            })
        });
        let loader = self.loader_pages().map(move |phys| self.leaf(phys, self.cache_of(phys, PAGE_2M)));
        low.chain(scanout).chain(fine).chain(loader)
    }

    fn in_scanout(&self, phys: u64, size: u64) -> bool {
        self.scanout.is_some_and(|(base, len)| phys >= base && phys + size <= base + len)
    }

    fn is_split(&self, page: u64) -> bool {
        self.fine_tables().contains(&page)
    }

    /// What a leaf is: the scanout where the scanout covers it, and what the
    /// typing says everywhere else.
    fn cache_of(&self, phys: u64, size: u64) -> Cache {
        if self.in_scanout(phys, size) {
            return Cache::Scanout;
        }
        self.typing.of(phys, size).expect("`new` typed every leaf")
    }

    fn directory_slot(&self, phys: u64) -> (usize, usize) {
        let directory = self
            .directories()
            .iter()
            .position(|gib| *gib == phys / GIB)
            .expect("every entry's GiB was claimed");
        (directory, ((phys / PAGE_2M) % 512) as usize)
    }

    fn leaf(&self, phys: u64, cache: Cache) -> Entry {
        let (directory, index) = self.directory_slot(phys);
        Entry { phys, slot: Slot::Directory { directory, index }, cache }
    }

    /// Name the directory for `gib`, unless it is already named. Infallible:
    /// `new` counts and refuses before it claims anything.
    fn claim(&mut self, gib: u64) {
        if self.gibs[..self.directories].contains(&gib) {
            return;
        }
        self.gibs[self.directories] = gib;
        self.directories += 1;
    }
}

/// `base..base + len` as whole 2 MiB pages, rounded out both ways: `(first
/// page, bytes covered)`. An empty range has no page to be in and is refused.
fn whole_pages(base: u64, len: u64) -> Result<(u64, u64), Refusal> {
    if len == 0 {
        return Err(Refusal::Extent { base, len });
    }
    let first = base / PAGE_2M * PAGE_2M;
    let end = base.checked_add(len).ok_or(Refusal::Extent { base, len })?;
    let end = end.checked_next_multiple_of(PAGE_2M).ok_or(Refusal::Extent { base, len })?;
    within_window(end)?;
    Ok((first, end - first))
}

/// Whether a range ending at `end` is one both views can hold.
fn within_window(end: u64) -> Result<(), Refusal> {
    if end > DIRECT_MAP_WINDOW {
        return Err(Refusal::PastWindow(end));
    }
    Ok(())
}

/// The directories past the low map's that the ranges `(first, end)` need
/// between them, each GiB counted once however many ranges fall in it.
fn fresh_directories(spans: [Option<(u64, u64)>; 2]) -> usize {
    let above = |span: Option<(u64, u64)>| {
        let (first, end) = span?;
        let (low, high) = ((first / GIB).max(LOW_DIRECTORIES as u64), (end - 1) / GIB);
        (low <= high).then_some((low, high))
    };
    let [a, b] = spans.map(above);
    let count = |span: Option<(u64, u64)>| span.map_or(0, |(low, high)| high - low + 1);
    let shared = match (a, b) {
        (Some(a), Some(b)) => count(Some((a.0.max(b.0), a.1.min(b.1))).filter(|(low, high)| low <= high)),
        _ => 0,
    };
    (count(a) + count(b) - shared) as usize
}
