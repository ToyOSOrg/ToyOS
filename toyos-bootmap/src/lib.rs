//! The bootloader's transient page tables, decided rather than built.
//!
//! Between the loader's `mov cr3` and the kernel's `mm::init` there is one
//! mapping in the machine, and everything the kernel touches in that window has
//! to be in it — its own image, the boot parameter, and the panel it reports a
//! wedge on. What goes where is arithmetic over three numbers, and the machine
//! that gets it wrong is the one that cannot say so: the panel is the thing
//! that went missing. So the arithmetic lives here, where a host test can ask
//! it about a framebuffer at 256 GiB without owning a laptop that has one.
//!
//! Pure: three numbers in, a [`Plan`] out. The loader allocates the pages and
//! writes the entries.

#![no_std]
#![forbid(unsafe_code)]

use core::fmt;

/// The page every entry in this map describes.
pub const PAGE_2M: u64 = 2 * 1024 * 1024;

const GIB: u64 = 1 << 30;

/// One PDPT reaches 512 GiB, and the map has two: the identity view at PML4[0]
/// and the high-half view at PML4[256].
const GIB_PER_PDPT: u64 = 512;

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

/// The pool a builder needs: a PML4, a PDPT per view, and every directory.
pub const MAX_PAGES: usize = 3 + MAX_DIRECTORIES;

/// Why a machine's memory does not fit these tables.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
    /// A 2 MiB page cannot begin anywhere else, and rounding the base down
    /// would retype memory below it that firmware handed to somebody else.
    Unaligned(u64),
    /// The range's own end does not fit an address.
    Extent { base: u64, len: u64 },
    /// Past the reach of the two PDPTs this map has.
    PastPdpt(u64),
    /// More directories than [`MAX_DIRECTORIES`].
    Directories(usize),
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
                "GiB {gib} is past the {GIB_PER_PDPT} this map's two PDPTs reach"
            ),
            Self::Directories(needed) => {
                write!(f, "{needed} page directories are needed and {MAX_DIRECTORIES} may be named")
            }
        }
    }
}

/// What a 2 MiB entry selects.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cache {
    /// PAT entry 0, whose type is the MTRR's — plain memory.
    DeferToMtrr,
    /// PCD and PWT set with the PAT bit clear: PAT entry 3, uncacheable under
    /// every MTRR type and under an unprogrammed PAT, which is what the machine
    /// has until the kernel's `pat::init`.
    Uncacheable,
}

/// One 2 MiB entry the map holds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Entry {
    pub phys: u64,
    /// Which of [`Plan::directories`] holds it, by position.
    pub directory: usize,
    /// Its index in that directory.
    pub index: usize,
    pub cache: Cache,
}

/// Where a machine's memory and its scanout go in the loader's two views.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Plan {
    gibs: [u64; MAX_DIRECTORIES],
    directories: usize,
    scanout: Option<(u64, u64)>,
}

impl Plan {
    /// Lay out [`BOOT_MAP_BYTES`] and `scanout`, or say why neither can be.
    ///
    /// `scanout` is firmware's framebuffer as firmware reports it. Its base
    /// must be 2 MiB aligned; its length is rounded up, because a page cannot
    /// end anywhere else and what the last one covers past the framebuffer is
    /// the same aperture firmware put it in.
    pub fn new(scanout: Option<(u64, u64)>) -> Result<Self, Refusal> {
        let mut plan =
            Self { gibs: [0; MAX_DIRECTORIES], directories: 0, scanout: None };
        for gib in 0..BOOT_MAP_BYTES / GIB {
            plan.claim(gib)?;
        }
        let Some((base, len)) = scanout else { return Ok(plan) };
        if base % PAGE_2M != 0 {
            return Err(Refusal::Unaligned(base));
        }
        let end = base.checked_add(len).ok_or(Refusal::Extent { base, len })?;
        let covered = end
            .checked_next_multiple_of(PAGE_2M)
            .ok_or(Refusal::Extent { base, len })?
            - base;
        let mut phys = base;
        while phys < base + covered {
            plan.claim(phys / GIB)?;
            phys += PAGE_2M;
        }
        plan.scanout = Some((base, covered));
        Ok(plan)
    }

    /// The GiB each directory covers, in the order a builder allocates them.
    pub fn directories(&self) -> &[u64] {
        &self.gibs[..self.directories]
    }

    /// How many pool pages a builder needs for this plan.
    pub fn pages(&self) -> usize {
        3 + self.directories
    }

    /// The scanout as this map covers it: firmware's base, rounded up to whole
    /// pages. `None` is a machine with no framebuffer.
    pub fn scanout(&self) -> Option<(u64, u64)> {
        self.scanout
    }

    /// Every entry the map holds, low memory first and each place once: a
    /// scanout inside the low map retypes the pages already there rather than
    /// adding a second entry for them.
    pub fn entries(&self) -> impl Iterator<Item = Entry> + '_ {
        let plan = *self;
        let low = (0..BOOT_MAP_BYTES / PAGE_2M)
            .map(move |page| plan.entry(page * PAGE_2M, plan.cache_of(page * PAGE_2M)));
        let scanout = self.scanout.into_iter().flat_map(move |(base, covered)| {
            (0..covered / PAGE_2M)
                .map(move |page| base + page * PAGE_2M)
                .filter(|phys| *phys >= BOOT_MAP_BYTES)
                .map(move |phys| plan.entry(phys, Cache::Uncacheable))
        });
        low.chain(scanout)
    }

    /// What a page of the low map selects: the scanout's type where the two
    /// overlap, and plain memory everywhere else.
    fn cache_of(&self, phys: u64) -> Cache {
        match self.scanout {
            Some((base, covered)) if phys >= base && phys < base + covered => Cache::Uncacheable,
            _ => Cache::DeferToMtrr,
        }
    }

    fn entry(&self, phys: u64, cache: Cache) -> Entry {
        Entry {
            phys,
            directory: self
                .directories()
                .iter()
                .position(|gib| *gib == phys / GIB)
                .expect("every entry's GiB was claimed"),
            index: ((phys / PAGE_2M) % 512) as usize,
            cache,
        }
    }

    /// Name the directory for `gib`, unless it is already named.
    fn claim(&mut self, gib: u64) -> Result<(), Refusal> {
        if gib >= GIB_PER_PDPT {
            return Err(Refusal::PastPdpt(gib));
        }
        if self.gibs[..self.directories].contains(&gib) {
            return Ok(());
        }
        if self.directories == MAX_DIRECTORIES {
            return Err(Refusal::Directories(self.directories + 1));
        }
        self.gibs[self.directories] = gib;
        self.directories += 1;
        Ok(())
    }
}
