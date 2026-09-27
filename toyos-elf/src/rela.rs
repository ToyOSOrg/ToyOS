//! `Elf64_Rela` tables, as a view over bytes, and the parse that turns one
//! entry into a [`Reloc`] a loader may act on.
//!
//! A relocation is an instruction to write `width` bytes at a file-chosen
//! offset with a file-chosen value, so this is the trust boundary's sharpest
//! edge. A raw [`Rela`] exposes nothing but its kind: every number in it
//! reaches a loader only through [`parse`], which checks the destination
//! against the window the loader writes into and the value against the image
//! or TLS segment it must name, and answers with types that cannot hold
//! anything else ([`ImageOffset`], [`SymIndex`], [`TlsOffset`]).
//!
//! [`RelocKind::write_width`] is the one table the parse and the writers both
//! read: a type missing from it is a type nobody patches, and a type in it that
//! no writer handles would be checked for a write that never happens.

use crate::header::Machine;
use crate::layout::{Extent, ImageOffset, TlsSegment};
use crate::read;
use crate::sym::SymIndex;
use crate::tls::TlsOffset;

/// Bytes in one `Elf64_Rela`.
pub const ENTRY_SIZE: usize = 24;

/// The dynamic relocations this loader knows about, by what they ask for
/// rather than by any one machine's number: [`RelocKind::from_raw`] is the
/// only place a number is read.
///
/// `Other` carries the raw type rather than dropping it, so a log line can name
/// what it skipped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelocKind {
    /// `R_X86_64_GLOB_DAT`, `R_AARCH64_GLOB_DAT`
    GlobDat,
    /// `R_X86_64_JUMP_SLOT`, `R_AARCH64_JUMP_SLOT`
    JumpSlot,
    /// `R_X86_64_RELATIVE`, `R_AARCH64_RELATIVE`
    Relative,
    /// `R_X86_64_DTPMOD64`, `R_AARCH64_TLS_DTPMOD`
    DtpMod64,
    /// `R_X86_64_DTPOFF64`, `R_AARCH64_TLS_DTPREL`
    DtpOff64,
    /// `R_X86_64_TPOFF64`, `R_AARCH64_TLS_TPREL`
    Tpoff64,
    /// `R_X86_64_TPOFF32`; AArch64 has no 32-bit thread-pointer offset.
    Tpoff32,
    /// `R_AARCH64_TLSDESC`: a TLS descriptor, whose resolver this loader does
    /// not have, so [`parse`] refuses it.
    TlsDesc,
    Other(u32),
}

impl RelocKind {
    pub const fn from_raw(machine: Machine, r_type: u32) -> RelocKind {
        match (machine, r_type) {
            (Machine::X86_64, 6) | (Machine::Aarch64, 1025) => RelocKind::GlobDat,
            (Machine::X86_64, 7) | (Machine::Aarch64, 1026) => RelocKind::JumpSlot,
            (Machine::X86_64, 8) | (Machine::Aarch64, 1027) => RelocKind::Relative,
            (Machine::X86_64, 16) | (Machine::Aarch64, 1028) => RelocKind::DtpMod64,
            (Machine::X86_64, 17) | (Machine::Aarch64, 1029) => RelocKind::DtpOff64,
            (Machine::X86_64, 18) | (Machine::Aarch64, 1030) => RelocKind::Tpoff64,
            (Machine::X86_64, 23) => RelocKind::Tpoff32,
            (Machine::Aarch64, 1031) => RelocKind::TlsDesc,
            (_, other) => RelocKind::Other(other),
        }
    }

    /// How many bytes the loader writes for this type, or `None` for one it
    /// never writes.
    pub const fn write_width(self) -> Option<u64> {
        match self {
            RelocKind::GlobDat
            | RelocKind::JumpSlot
            | RelocKind::Relative
            | RelocKind::DtpMod64
            | RelocKind::DtpOff64
            | RelocKind::Tpoff64 => Some(8),
            RelocKind::Tpoff32 => Some(4),
            RelocKind::TlsDesc | RelocKind::Other(_) => None,
        }
    }
}

/// One `Elf64_Rela` as the file wrote it. Only its kind is readable: the
/// numbers in it are the file's, and they leave this module through [`parse`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rela {
    offset: u64,
    sym: u32,
    kind: RelocKind,
    addend: i64,
}

impl Rela {
    pub const fn kind(&self) -> RelocKind {
        self.kind
    }
}

/// A relocation table, addressed by entry rather than by byte.
///
/// Holds no ownership and reserves nothing: `DT_RELASZ` is a length the file
/// chose, and the only honest bound on it is the bytes the caller already has.
#[derive(Clone, Copy, Debug)]
pub struct RelaTable<'a> {
    data: &'a [u8],
    machine: Machine,
}

impl<'a> RelaTable<'a> {
    /// `machine` decides what each entry's type number means.
    pub const fn new(data: &'a [u8], machine: Machine) -> RelaTable<'a> {
        RelaTable { data, machine }
    }

    /// Whole entries the bytes hold. A trailing partial entry is not an entry.
    pub const fn len(&self) -> usize {
        self.data.len() / ENTRY_SIZE
    }

    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn get(&self, i: usize) -> Option<Rela> {
        let off = i.checked_mul(ENTRY_SIZE)?;
        let [r_type, r_sym] = read::u32_pair_at(self.data, off.checked_add(8)?)?;
        Some(Rela {
            offset: read::u64_at(self.data, off)?,
            sym: r_sym,
            kind: RelocKind::from_raw(self.machine, r_type),
            addend: read::i64_at(self.data, off.checked_add(16)?)?,
        })
    }

    /// Takes `self` by value — the table is a `Copy` view, so the iterator
    /// borrows the bytes rather than the caller's handle on them.
    pub fn iter(self) -> impl Iterator<Item = Rela> + 'a {
        (0..self.len()).filter_map(move |i| self.get(i))
    }
}

/// What one module's relocations are parsed against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rules {
    /// The image a `RELATIVE` value must point into.
    pub extent: Extent,
    /// Where writes may land, `[lo, hi)` in `r_offset`'s own coordinates.
    pub window: (u64, u64),
    /// Entries in the symbol table `r_sym` indexes.
    pub sym_count: usize,
    /// `Some` for a chunked writer (the exe), which drops a write crossing a
    /// fill page and so has it refused; `None` for a contiguous one.
    pub fill: Option<FillLattice>,
    /// The module's own `PT_TLS`, which a TLS relocation with `r_sym == 0`
    /// offsets into; `None` for a module with none.
    pub tls: Option<TlsSegment>,
}

/// A relocation whose destination lies in its window and whose value names
/// what its kind says it names: made only by [`parse`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reloc {
    offset: u64,
    op: Op,
}

impl Reloc {
    /// `r_offset`: `[offset, offset + width)` lies inside the window it was
    /// parsed against, and inside one fill page for a chunked writer.
    pub const fn offset(&self) -> u64 {
        self.offset
    }

    pub const fn op(&self) -> Op {
        self.op
    }
}

/// What a [`Reloc`] writes, per psABI, with every file-chosen number already
/// bounded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    /// `B + A`, 8 bytes: the address the image was placed at plus a position
    /// inside it.
    Relative(ImageOffset),
    /// `S`, 8 bytes: `GLOB_DAT` and `JUMP_SLOT`, bound to a symbol by name.
    Bind(SymIndex),
    /// `S + A - tp`, 8 bytes.
    Tpoff64(TlsRef),
    /// `S + A - tp`, 4 bytes, sign-extended by the instruction that reads it.
    Tpoff32(TlsRef),
    /// The id of the module defining the symbol, 8 bytes; `None` is `r_sym ==
    /// 0`, the relocating module itself.
    DtpMod64(Option<SymIndex>),
    /// `S + A` within its module's TLS block, 8 bytes.
    DtpOff64(TlsRef),
}

/// The `S + A` of a TLS relocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TlsRef {
    /// `r_sym == 0`: an offset into the relocating module's own TLS segment.
    Own(TlsOffset),
    /// A symbol the loader resolves by name, and the addend it is offset by.
    Symbol(TlsSymRef),
}

/// A TLS relocation's symbol and addend: the sum is bounded only once the
/// symbol resolves, against the TLS segment of the module defining it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TlsSymRef {
    sym: SymIndex,
    addend: i64,
}

impl TlsSymRef {
    pub const fn sym(self) -> SymIndex {
        self.sym
    }

    /// The addend `A`, resolved against the defining symbol's `S` through
    /// [`crate::Sym::tls_offset`].
    pub const fn addend(self) -> i64 {
        self.addend
    }
}

/// Parse one entry against the module's [`Rules`]: `Ok(None)` for a type this
/// loader does not write, the reason for one it must not.
///
/// The window is the *writable* one rather than the whole image: once the
/// module is cached its read-only pages are shared between processes, and the
/// write lands in a private allocation covering only that window.
pub fn parse(rela: Rela, rules: &Rules) -> Result<Option<Reloc>, RelocError> {
    let Some(width) = rela.kind.write_width() else {
        return match rela.kind {
            RelocKind::TlsDesc => Err(RelocError::TlsDescriptor),
            _ => Ok(None),
        };
    };
    let (lo, hi) = rules.window;
    let end = rela.offset.checked_add(width).ok_or(RelocError::OffsetOverflows)?;
    if rela.offset < lo || end > hi {
        return Err(RelocError::OutsideWindow);
    }
    if let Some(fill) = rules.fill {
        let within = rela
            .offset
            .wrapping_sub(fill.base)
            .checked_rem(fill.granule)
            .ok_or(RelocError::StraddlesFillPage)?;
        if within.checked_add(width).is_none_or(|e| e > fill.granule) {
            return Err(RelocError::StraddlesFillPage);
        }
    }

    let sym = || SymIndex::below(rela.sym, rules.sym_count).ok_or(RelocError::SymbolPastTable);
    let tls = || -> Result<TlsRef, RelocError> {
        if rela.sym != 0 {
            return Ok(TlsRef::Symbol(TlsSymRef { sym: sym()?, addend: rela.addend }));
        }
        rules
            .tls
            .and_then(|segment| TlsOffset::of(0, rela.addend, segment))
            .map(TlsRef::Own)
            .ok_or(RelocError::TlsOutsideSegment)
    };
    let op = match rela.kind {
        RelocKind::Relative => Op::Relative(
            u64::try_from(rela.addend)
                .ok()
                .and_then(|a| rules.extent.offset(a))
                .ok_or(RelocError::RelativeOutsideImage)?,
        ),
        RelocKind::GlobDat | RelocKind::JumpSlot => Op::Bind(sym()?),
        RelocKind::Tpoff64 => Op::Tpoff64(tls()?),
        RelocKind::Tpoff32 => Op::Tpoff32(tls()?),
        RelocKind::DtpMod64 => Op::DtpMod64(if rela.sym == 0 { None } else { Some(sym()?) }),
        RelocKind::DtpOff64 => Op::DtpOff64(tls()?),
        RelocKind::TlsDesc | RelocKind::Other(_) => return Ok(None),
    };
    Ok(Some(Reloc { offset: rela.offset, op }))
}

/// How many entries of each kind a set of tables holds.
///
/// The loader reserves exactly from these instead of letting a `Vec` double:
/// `DT_RELASZ` and `DT_PLTRELSZ` are bounded separately, so two individually
/// acceptable tables sum to a collection no bound on either input can catch,
/// and growth-by-doubling then overshoots that sum as well.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RelaCounts {
    pub relative: usize,
    pub bind: usize,
    pub tpoff64: usize,
    pub tpoff32: usize,
    pub dtpmod64: usize,
    pub dtpoff64: usize,
    pub tlsdesc: usize,
}

impl RelaCounts {
    pub fn of(entries: impl Iterator<Item = Rela>) -> RelaCounts {
        let mut counts = RelaCounts::default();
        for rela in entries {
            let slot = match rela.kind {
                RelocKind::Relative => &mut counts.relative,
                RelocKind::GlobDat | RelocKind::JumpSlot => &mut counts.bind,
                RelocKind::Tpoff64 => &mut counts.tpoff64,
                RelocKind::Tpoff32 => &mut counts.tpoff32,
                RelocKind::DtpMod64 => &mut counts.dtpmod64,
                RelocKind::DtpOff64 => &mut counts.dtpoff64,
                RelocKind::TlsDesc => &mut counts.tlsdesc,
                RelocKind::Other(_) => continue,
            };
            *slot += 1;
        }
        counts
    }

    /// The largest of the counts a caller actually reserves for.
    ///
    /// Deliberately not "the largest count": a ceiling on a kind nothing
    /// stores refuses a file over a collection that does not exist, and a
    /// library is about 99.5 % `RELATIVE` — so a cache that bounded itself on
    /// `relative`, which it keeps none of, would refuse to cache every real
    /// library in the tree.
    pub fn max_of(&self, kinds: &[RelocKind]) -> usize {
        kinds.iter().map(|&k| self.count_of(k)).max().unwrap_or(0)
    }

    /// What an executable's loader reserves for each group it keeps, at `width`
    /// bytes an entry, or why it keeps none: a TLS descriptor, which only a
    /// resolver this loader does not have can fill, or a group that would
    /// not fit `max_bytes`. The reservation is had only through this refusal.
    pub fn for_executable(&self, width: usize, max_bytes: usize) -> Result<ExeReservation, ExeRefusal> {
        if self.tlsdesc != 0 {
            return Err(ExeRefusal::TlsDescriptor);
        }
        let kept = [RelocKind::Relative, RelocKind::GlobDat, RelocKind::Tpoff64, RelocKind::Tpoff32];
        if self.max_of(&kept).checked_mul(width).is_none_or(|b| b > max_bytes) {
            return Err(ExeRefusal::TooLarge);
        }
        Ok(ExeReservation {
            relative: self.relative,
            bind: self.bind,
            tpoff64: self.tpoff64,
            tpoff32: self.tpoff32,
        })
    }

    pub fn count_of(&self, kind: RelocKind) -> usize {
        match kind {
            RelocKind::Relative => self.relative,
            RelocKind::GlobDat | RelocKind::JumpSlot => self.bind,
            RelocKind::Tpoff64 => self.tpoff64,
            RelocKind::Tpoff32 => self.tpoff32,
            RelocKind::DtpMod64 => self.dtpmod64,
            RelocKind::DtpOff64 => self.dtpoff64,
            RelocKind::TlsDesc => self.tlsdesc,
            RelocKind::Other(_) => 0,
        }
    }
}

/// The lattice a chunked writer applies relocations in: a write must land
/// wholly within one page, since the page-at-a-time applier never revisits a
/// tail handed to the next page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FillLattice {
    /// Based at the image's `vaddr_min`; each write lies within one `granule`.
    pub base: u64,
    pub granule: u64,
}

/// The demand-fault page an executable's relocations are filled in.
pub const FILL_GRANULE: u64 = 4096;

/// How many entries of each group an executable's loader keeps: made only by
/// [`RelaCounts::for_executable`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ExeReservation {
    pub relative: usize,
    /// `GLOB_DAT` and `JUMP_SLOT`.
    pub bind: usize,
    pub tpoff64: usize,
    pub tpoff32: usize,
}

/// Why an executable's relocations are refused before any is kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExeRefusal {
    /// [`RelocError::TlsDescriptor`]'s reason.
    TlsDescriptor,
    /// A group would not fit one allocation.
    TooLarge,
}

impl ExeRefusal {
    pub const fn as_str(self) -> &'static str {
        match self {
            ExeRefusal::TlsDescriptor => RelocError::TlsDescriptor.as_str(),
            ExeRefusal::TooLarge => "ELF: a relocation group does not fit one allocation",
        }
    }
}

/// Why a relocation cannot be applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelocError {
    /// `r_offset + width` does not fit a `u64`.
    OffsetOverflows,
    /// The write would land outside the window the loader owns.
    OutsideWindow,
    /// `r_sym` names an entry past the end of `.dynsym`.
    SymbolPastTable,
    /// The write would cross a fill page, so a chunked writer would drop it.
    StraddlesFillPage,
    /// A TLS descriptor, which only a resolver this loader does not have can
    /// fill.
    TlsDescriptor,
    /// A `RELATIVE` addend that is no address inside the image: psABI `B + A`
    /// with `A` a link-time address, and the image is all this module has.
    RelativeOutsideImage,
    /// A TLS offset outside the TLS segment it names, or in a module that has
    /// none.
    TlsOutsideSegment,
    /// A thread-pointer offset no machine word, or for `TPOFF32` no 32-bit
    /// field, holds.
    TpoffOverflows,
}

impl RelocError {
    pub const fn as_str(self) -> &'static str {
        match self {
            RelocError::OffsetOverflows => "ELF: relocation r_offset + width overflows",
            RelocError::OutsideWindow => "ELF: relocation r_offset outside the writable image",
            RelocError::SymbolPastTable => "ELF: relocation r_sym past .dynsym",
            RelocError::StraddlesFillPage => "ELF: relocation crosses a fill-page boundary",
            RelocError::TlsDescriptor => "ELF: R_AARCH64_TLSDESC has no resolver in this loader",
            RelocError::RelativeOutsideImage => "ELF: RELATIVE addend is no address inside the image",
            RelocError::TlsOutsideSegment => "ELF: TLS relocation names an offset outside its PT_TLS",
            RelocError::TpoffOverflows => "ELF: TPOFF value does not fit the field it is written to",
        }
    }
}

impl core::fmt::Display for RelocError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The tables a loader reads *while* it is writing relocations, as
/// image-relative `[start, end)`. An absent table is an empty range.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReadTables {
    pub dynsym: (u64, u64),
    pub dynstr: (u64, u64),
    pub rela: (u64, u64),
    pub jmprel: (u64, u64),
}

/// Refuse an image that puts a table the loader reads on a `page` of the
/// window relocations may write into.
///
/// A loader resolving symbols holds a `&[u8]` over `.dynsym` and `.dynstr`, and
/// one over each relocation table it is iterating, across writes into the same
/// allocation — and it parses those tables again after the image is mapped,
/// where a page the window touches is one the process may write. Disjointness
/// from every such page is what makes those borrows sound and the second parse
/// the first one's answer.
///
/// **A conforming image never triggers this.** The ELF gABI gives `.dynsym`,
/// `.dynstr`, `.rela.dyn` and `.rela.plt` `SHF_ALLOC` without `SHF_WRITE`, so a
/// linker places them in a non-writable segment, on pages of its own.
pub fn tables_outside_window(
    tables: &ReadTables,
    window: (u64, u64),
    page: u64,
) -> Result<(), &'static str> {
    if window.1 <= window.0 {
        return Ok(());
    }
    let mask = page.wrapping_sub(1);
    let window = (window.0 & !mask, window.1.checked_add(mask).map_or(u64::MAX, |e| e & !mask));
    for (range, refusal) in [
        (tables.dynsym, "ELF: .dynsym lies on a page of the module's writable window"),
        (tables.dynstr, "ELF: .dynstr lies on a page of the module's writable window"),
        (tables.rela, "ELF: .rela.dyn lies on a page of the module's writable window"),
        (tables.jmprel, "ELF: .rela.plt lies on a page of the module's writable window"),
    ] {
        if range.1 > range.0 && range.0 < window.1 && window.0 < range.1 {
            return Err(refusal);
        }
    }
    Ok(())
}
