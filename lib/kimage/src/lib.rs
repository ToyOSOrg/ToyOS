//! The kernel section of a signed image (`toyos_update::image::KERNEL`): the
//! kernel laid out by the build at its own addresses, the `RELATIVE`
//! relocations the loader applies once it has placed it, the protection of
//! each range, its symbols, and the supervisor's program
//! (`toyos_abi::syscall::SpawnMap`s over the supervisor section). The build
//! reads the kernel's and the supervisor's ELF with `toyos::elf`; the loader
//! and the kernel read only this.
//!
//! ```text
//! header      magic "TOYOSKRN" | format u32 | e_machine u16 | 0 u16
//!             entry u64 | memory u64 | relro [lo, hi) u64 u64
//!             ranges u32 | relocations u32 | symbols u32 | names u32 | image_at u64
//! ranges      [lo u64 | hi u64 | VmProt u64], page-aligned, ascending, disjoint
//! relocations [offset u64 | addend u64]: base + addend, 8 bytes at offset
//! symbols     [addr u64 | size u64 | name u32 | len u32], ascending by addr
//! names       the symbols' names
//! supervisor  entry u64 | image_base u64 | build-id len u64 | build-id [32] | maps u64
//!             [SpawnMap], `object` 0 for the supervisor section
//! image       at image_at, a page boundary: the image's bytes from address 0,
//!             zeros past them to `memory`
//! ```
//!
//! Little-endian throughout. The section is signed, so a malformed one is the
//! build's bug: [`Kernel::parse`] refuses it by name, as a bug fence, rather
//! than trusting its counts.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

mod write;
pub use write::{Input, Symbol};

use toyos_abi::syscall::{SpawnMap, VmProt, MAX_BUILD_ID, MAX_SPAWN_MAPS};

pub const MAGIC: [u8; 8] = *b"TOYOSKRN";
pub const FORMAT: u32 = 1;
const HEADER: usize = 72;
const RANGE: usize = 24;
const RELOCATION: usize = 16;
const SYMBOL: usize = 24;
const PROGRAM: usize = 64;
const MAP: usize = core::mem::size_of::<SpawnMap>();
pub const PAGE: u64 = 4096;

/// One range of the image and what its pages may be used for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Range {
    pub lo: u64,
    pub hi: u64,
    pub prot: VmProt,
}

/// A kernel section, its every count and offset checked against its bytes.
#[derive(Clone, Copy, Debug)]
pub struct Kernel<'a> {
    pub machine: u16,
    /// The entry's address in the image.
    pub entry: u64,
    /// Bytes the image occupies from address 0, whole pages.
    pub memory: u64,
    /// `PT_GNU_RELRO`'s whole pages, read-only once relocated.
    pub relro: (u64, u64),
    ranges: &'a [u8],
    relocations: &'a [u8],
    pub symbols: Symbols<'a>,
    pub supervisor: Supervisor<'a>,
    /// The image's bytes from address 0, at most `memory`.
    pub image: &'a [u8],
}

fn u64_at(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(bytes.get(at..at.checked_add(8)?)?.try_into().ok()?))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at.checked_add(4)?)?.try_into().ok()?))
}

/// `count` records of `size` bytes from `*at`, moving `*at` past them.
fn take<'a>(bytes: &'a [u8], at: &mut usize, count: usize, size: usize) -> Option<&'a [u8]> {
    let end = count.checked_mul(size)?.checked_add(*at)?;
    let taken = bytes.get(*at..end)?;
    *at = end;
    Some(taken)
}

impl<'a> Kernel<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, &'static str> {
        const SHORT: &str = "kernel image: a table past the section's end";
        if bytes.get(..8) != Some(&MAGIC[..]) {
            return Err("kernel image: not TOYOSKRN");
        }
        if u32_at(bytes, 8) != Some(FORMAT) {
            return Err("kernel image: another format");
        }
        let field = |at| u64_at(bytes, at).ok_or(SHORT);
        let count = |at| u32_at(bytes, at).map(|n| n as usize).ok_or(SHORT);
        let machine = u16::from_le_bytes([bytes.get(12).copied().ok_or(SHORT)?, bytes.get(13).copied().ok_or(SHORT)?]);
        let (entry, memory, relro) = (field(16)?, field(24)?, (field(32)?, field(40)?));
        let mut at = HEADER;
        let ranges = take(bytes, &mut at, count(48)?, RANGE).ok_or(SHORT)?;
        let relocations = take(bytes, &mut at, count(52)?, RELOCATION).ok_or(SHORT)?;
        let entries = take(bytes, &mut at, count(56)?, SYMBOL).ok_or(SHORT)?;
        let names = take(bytes, &mut at, count(60)?, 1).ok_or(SHORT)?;
        let program = take(bytes, &mut at, 1, PROGRAM).ok_or(SHORT)?;
        let maps = usize::try_from(u64_at(program, 56).ok_or(SHORT)?).map_err(|_| SHORT)?;
        if maps > MAX_SPAWN_MAPS {
            return Err("kernel image: more supervisor ranges than a spawn carries");
        }
        let maps = take(bytes, &mut at, maps, MAP).ok_or(SHORT)?;
        let id_len = u64_at(program, 16).filter(|&n| n <= MAX_BUILD_ID as u64).ok_or("kernel image: a build-id past its bound")?;
        let image_at = usize::try_from(field(64)?).map_err(|_| SHORT)?;
        let image = bytes.get(image_at..).filter(|_| image_at >= at && (image_at as u64).is_multiple_of(PAGE)).ok_or(SHORT)?;
        let kernel = Kernel {
            machine,
            entry,
            memory,
            relro,
            ranges,
            relocations,
            symbols: Symbols { entries, names },
            supervisor: Supervisor {
                entry: u64_at(program, 0).ok_or(SHORT)?,
                image_base: u64_at(program, 8).ok_or(SHORT)?,
                build_id: &program[24..24 + id_len as usize],
                maps,
            },
            image,
        };
        kernel.check()
    }

    fn check(self) -> Result<Self, &'static str> {
        if !self.memory.is_multiple_of(PAGE) || self.image.len() as u64 > self.memory {
            return Err("kernel image: its bytes are not whole pages of its memory");
        }
        let mut covered = 0;
        for range in self.ranges() {
            let range = range.ok_or("kernel image: a range with no protection")?;
            if range.lo < covered || range.hi <= range.lo || range.hi > self.memory || !range.lo.is_multiple_of(PAGE) || !range.hi.is_multiple_of(PAGE) {
                return Err("kernel image: ranges that are not ascending whole pages of its memory");
            }
            covered = range.hi;
        }
        self.text()?;
        if self.entry >= self.memory {
            return Err("kernel image: an entry past its memory");
        }
        let (lo, hi) = self.relro;
        if lo > hi || hi > self.memory || !lo.is_multiple_of(PAGE) || !hi.is_multiple_of(PAGE) {
            return Err("kernel image: RELRO that is not whole pages of its memory");
        }
        if self.relocations().any(|(offset, addend)| offset.checked_add(8).is_none_or(|end| end > self.memory) || addend > self.memory) {
            return Err("kernel image: a relocation outside its memory");
        }
        self.symbols.check()?;
        Ok(self)
    }

    /// Every range and its protection; `None` for a word that names none.
    pub fn ranges(&self) -> impl Iterator<Item = Option<Range>> + 'a {
        self.ranges.as_chunks::<RANGE>().0.iter().map(|r| {
            let (lo, hi) = (u64_at(r, 0)?, u64_at(r, 8)?);
            Some(Range { lo, hi, prot: VmProt::from_u64(u64_at(r, 16)?)? })
        })
    }

    /// The one executable range, which no writable byte shares a page with.
    pub fn text(&self) -> Result<Range, &'static str> {
        let mut executable = self.ranges().flatten().filter(|r| r.prot == VmProt::ReadExec);
        match (executable.next(), executable.next()) {
            (Some(text), None) => Ok(text),
            _ => Err("kernel image: not exactly one executable range"),
        }
    }

    /// `(offset, addend)`: the loader writes its base plus `addend` at `offset`.
    pub fn relocations(&self) -> impl Iterator<Item = (u64, u64)> + 'a {
        self.relocations.as_chunks::<RELOCATION>().0.iter().filter_map(|r| Some((u64_at(r, 0)?, u64_at(r, 8)?)))
    }
}

/// The kernel's functions, each at the address nearest below an address that
/// names it.
#[derive(Clone, Copy, Debug)]
pub struct Symbols<'a> {
    entries: &'a [u8],
    names: &'a [u8],
}

impl<'a> Symbols<'a> {
    pub fn count(&self) -> usize {
        self.entries.len() / SYMBOL
    }

    fn entry(&self, i: usize) -> Option<(u64, u64, &'a str)> {
        let e = self.entries.get(i.checked_mul(SYMBOL)?..)?;
        let (name, len) = (u32_at(e, 16)? as usize, u32_at(e, 20)? as usize);
        let name = core::str::from_utf8(self.names.get(name..name.checked_add(len)?)?).ok()?;
        Some((u64_at(e, 0)?, u64_at(e, 8)?, name))
    }

    fn check(&self) -> Result<(), &'static str> {
        let mut last = 0;
        for i in 0..self.count() {
            let (addr, _, _) = self.entry(i).ok_or("kernel image: a symbol whose name is past the names")?;
            if addr <= last && i != 0 {
                return Err("kernel image: symbols not ascending");
            }
            last = addr;
        }
        Ok(())
    }

    /// The function `offset` falls in, and how far in: the nearest at or
    /// below it, owning every address up to the next if it has no size, and
    /// none past its last byte if it has one. No allocation, lock or panic: a
    /// panic handler and an NMI read it.
    pub fn resolve(&self, offset: u64) -> Option<(&'a str, u64)> {
        let (mut lo, mut hi) = (0, self.count());
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            match self.entry(mid)?.0 <= offset {
                true => lo = mid + 1,
                false => hi = mid,
            }
        }
        let (addr, size, name) = self.entry(lo.checked_sub(1)?)?;
        let within = offset - addr;
        (!(size > 0 && within >= size) && !name.is_empty()).then_some((name, within))
    }
}

/// The supervisor's program, as the build laid it out.
#[derive(Clone, Copy, Debug)]
pub struct Supervisor<'a> {
    pub entry: u64,
    pub image_base: u64,
    pub build_id: &'a [u8],
    maps: &'a [u8],
}

/// What a supervisor range's `object` names: the supervisor section.
pub const SUPERVISOR_OBJECT: u64 = 0;

impl Supervisor<'_> {
    /// Its ranges, `object` [`SUPERVISOR_OBJECT`] or `HANDLE_INVALID` for zeros.
    pub fn maps(&self) -> impl Iterator<Item = SpawnMap> + '_ {
        self.maps.as_chunks::<MAP>().0.iter().filter_map(|m| {
            Some(SpawnMap {
                addr: u64_at(m, 0)?,
                len: u64_at(m, 8)?,
                object: u64_at(m, 16)?,
                offset: u64_at(m, 24)?,
                copied: u64_at(m, 32)?,
                prot: u64_at(m, 40)?,
            })
        })
    }
}

/// A static image loaded at its own addresses into one allocation with its
/// stack after it, in offsets from the allocation's base.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StackedImage {
    /// Bytes to allocate: the image, then the stack.
    pub size: u64,
    /// Where the stack's lowest byte is; its top is `size`.
    pub stack: u64,
}

impl StackedImage {
    /// The image ending at `end` with a `stack_size`-byte stack after it, or
    /// `None` for one no address holds. The stack starts on the next page:
    /// both ABIs want the stack pointer 16-byte aligned, which AArch64's
    /// `SCTLR_EL1.SA` enforces on every access through it. `stack_size` is
    /// whole pages, so the top is too.
    pub fn place(end: u64, stack_size: u64) -> Option<StackedImage> {
        if !stack_size.is_multiple_of(PAGE) {
            return None;
        }
        let stack = end.checked_next_multiple_of(PAGE)?;
        Some(StackedImage { size: stack.checked_add(stack_size)?, stack })
    }
}
