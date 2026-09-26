//! The bootloader's transient page tables, decided rather than built.
//!
//! Between the loader's switch to these tables and the kernel's `mm::init`
//! there is one mapping in the machine, and everything the kernel touches in
//! that window has to be in it — its own image, the boot parameter, and the
//! panel it reports a wedge on.
//!
//! Both architectures walk the same shape — a root (x86-64's PML4, AArch64's
//! L0), a table per 512 GiB (PDPT, L1), a table per GiB (page directory, L2)
//! of 2 MiB leaves — so one [`Plan`] serves both. What differs is how a leaf
//! is typed ([`Typing`]) and how an entry is encoded ([`x86_64`],
//! [`aarch64`]).
//!
//! Pure: the scanout and, where the architecture types memory by firmware's
//! map, the write-back ranges in; a [`Plan`] out. The loader allocates the
//! pages and writes the entries.

#![no_std]
#![forbid(unsafe_code)]

use core::fmt;

pub mod aarch64;
pub mod x86_64;

/// The page every entry in this map describes.
pub const PAGE_2M: u64 = 2 * 1024 * 1024;

const GIB: u64 = 1 << 30;

/// One second-level table reaches 512 GiB, and the map has two: the identity
/// view at root slot 0 and the high-half view at root slot 256.
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

/// One page directory per GiB of [`BOOT_MAP_BYTES`].
const LOW_DIRECTORIES: usize = (BOOT_MAP_BYTES / GIB) as usize;

/// What a scanout adds: it lies inside one GiB or straddles two, and a wider
/// one is [`Refusal::Directories`] rather than a silent overrun.
const SCANOUT_DIRECTORIES: usize = 2;

/// Every page directory a [`Plan`] can name.
pub const MAX_DIRECTORIES: usize = LOW_DIRECTORIES + SCANOUT_DIRECTORIES;

/// The small page a split 2 MiB page is mapped in.
pub const PAGE_4K: u64 = 4096;

/// Leaves in one table.
const PAGES_PER_TABLE: u64 = PAGE_2M / PAGE_4K;

/// The 2 MiB pages a scanout covers only in part: at most its first and its last.
const FINE_TABLES: usize = 2;

/// The pool a builder needs: a root, a second-level table per view, every
/// directory, and a table of 4 KiB leaves per split page.
pub const MAX_PAGES: usize = 3 + MAX_DIRECTORIES + FINE_TABLES;

/// Why a machine's memory does not fit these tables.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
    /// A 2 MiB page cannot begin anywhere else, and rounding the base down
    /// would retype memory below it that firmware handed to somebody else.
    Unaligned(u64),
    /// The range's own end does not fit an address.
    Extent { base: u64, len: u64 },
    /// Past the reach of the two second-level tables this map has.
    PastPdpt(u64),
    /// More directories than [`MAX_DIRECTORIES`].
    Directories(usize),
    /// A 2 MiB page of the low map that is write-back memory in part and not
    /// in the rest: either type is wrong for some of it.
    Mixed(u64),
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
            Self::PastPdpt(gib) => write!(
                f,
                "GiB {gib} is past the {GIB_PER_PDPT} this map's two second-level tables reach"
            ),
            Self::Directories(needed) => {
                write!(f, "{needed} page directories are needed and {MAX_DIRECTORIES} may be named")
            }
            Self::Mixed(phys) => write!(
                f,
                "the 2 MiB page at {phys:#x} is part write-back memory and part not, so no one \
                 memory type is right for all of it"
            ),
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

/// Where a machine's memory and its scanout go in the loader's two views.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Plan<'a> {
    gibs: [u64; MAX_DIRECTORIES],
    directories: usize,
    /// The 2 MiB pages split into 4 KiB leaves, by base.
    fine: [u64; FINE_TABLES],
    fines: usize,
    scanout: Option<(u64, u64)>,
    typing: Typing<'a>,
}

impl<'a> Plan<'a> {
    /// Lay out [`BOOT_MAP_BYTES`] and `scanout`, typed by `typing`, or say why
    /// they cannot be.
    ///
    /// `scanout` is firmware's framebuffer as firmware reports it. Under
    /// [`Typing::Firmware`] its base must be 2 MiB aligned and its length is
    /// rounded up to whole 2 MiB pages, because what the last one covers past
    /// the framebuffer is the same aperture firmware put it in. Under
    /// [`Typing::ByMap`] it need only be 4 KiB aligned: a 2 MiB page it covers
    /// in part is split into 4 KiB leaves, so the scanout's type reaches no
    /// byte beside it — a framebuffer firmware carved out of RAM sits between
    /// memory other owners hold.
    pub fn new(scanout: Option<(u64, u64)>, typing: Typing<'a>) -> Result<Self, Refusal> {
        let mut plan = Self {
            gibs: [0; MAX_DIRECTORIES],
            directories: 0,
            fine: [0; FINE_TABLES],
            fines: 0,
            scanout: None,
            typing,
        };
        for gib in 0..BOOT_MAP_BYTES / GIB {
            plan.claim(gib);
        }
        if let Some((base, len)) = scanout {
            plan.place_scanout(base, len)?;
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
        Ok(plan)
    }

    /// Claim the directories and the split pages `base..base + len` needs,
    /// and record the extent the map gives it.
    fn place_scanout(&mut self, base: u64, len: u64) -> Result<(), Refusal> {
        let granule = match self.typing {
            Typing::Firmware => PAGE_2M,
            Typing::ByMap(_) => PAGE_4K,
        };
        if !base.is_multiple_of(granule) {
            return Err(Refusal::Unaligned(base));
        }
        let end = base.checked_add(len).ok_or(Refusal::Extent { base, len })?;
        let end = end.checked_next_multiple_of(granule).ok_or(Refusal::Extent { base, len })?;
        let first = base / PAGE_2M * PAGE_2M;
        let last = (end - 1) / PAGE_2M * PAGE_2M;
        if last / GIB >= GIB_PER_PDPT {
            return Err(Refusal::PastPdpt(last / GIB));
        }
        // Counted whole before one is claimed, so a machine needing fourteen is
        // told fourteen rather than that one more than the budget was wanted.
        let low = LOW_DIRECTORIES as u64;
        let fresh = if last / GIB < low { 0 } else { last / GIB - (first / GIB).max(low) + 1 };
        let required = LOW_DIRECTORIES + fresh as usize;
        if required > MAX_DIRECTORIES {
            return Err(Refusal::Directories(required));
        }
        let mut page = first;
        while page <= last {
            self.claim(page / GIB);
            page += PAGE_2M;
        }
        // At most the first and the last page are covered in part.
        for page in [first, last] {
            let whole = base <= page && page + PAGE_2M <= end;
            if !whole && !self.is_split(page) {
                self.fine[self.fines] = page;
                self.fines += 1;
            }
        }
        self.scanout = Some((base, end - base));
        Ok(())
    }

    /// The GiB each directory covers, in the order a builder allocates them.
    pub fn directories(&self) -> &[u64] {
        &self.gibs[..self.directories]
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

    /// Every leaf the map holds, low memory first and each place once: a
    /// scanout inside the low map retypes the leaves already there rather than
    /// adding a second one for them.
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
        low.chain(scanout).chain(fine)
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
