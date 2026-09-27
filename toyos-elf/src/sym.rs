//! `Elf64_Sym` tables, as a view over bytes.
//!
//! [`SymTab::get`] answers `None` past the end rather than reading a partial
//! record, and a [`Sym`]'s `st_value` is readable only as what it names: an
//! [`ImageOffset`] inside its module ([`Sym::address`]) or a [`TlsOffset`]
//! inside its module's TLS segment ([`Sym::tls_offset`]).

use crate::layout::{Extent, ImageOffset, TlsSegment};
use crate::read;
use crate::tls::TlsOffset;
use crate::Error;

/// Bytes in one `Elf64_Sym`.
pub const ENTRY_SIZE: usize = 24;

pub const STB_GLOBAL: u8 = 1;
pub const STB_WEAK: u8 = 2;
pub const STT_FUNC: u8 = 2;
pub const STT_TLS: u8 = 6;

/// An `r_sym` below the symbol count it was parsed against: made only by the
/// relocation parse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SymIndex(u32);

impl SymIndex {
    pub(crate) fn below(raw: u32, count: usize) -> Option<SymIndex> {
        (widen(raw) < count).then_some(SymIndex(raw))
    }

    pub fn get(self) -> usize {
        widen(self.0)
    }
}

fn widen(v: u32) -> usize {
    usize::try_from(v).unwrap_or(usize::MAX)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sym {
    name: u32,
    info: u8,
    shndx: u16,
    value: u64,
    size: u64,
}

impl Sym {
    /// `st_shndx != SHN_UNDEF`: this module defines the symbol rather than
    /// importing it.
    pub const fn is_defined(&self) -> bool {
        self.shndx != 0
    }

    pub const fn bind(&self) -> u8 {
        self.info >> 4
    }

    pub const fn kind(&self) -> u8 {
        self.info & 0xf
    }

    /// Whether another module may bind to this symbol.
    pub const fn is_exported(&self) -> bool {
        self.is_defined() && matches!(self.bind(), STB_GLOBAL | STB_WEAK)
    }

    /// This symbol's name in `strings`, `""` when it names nothing readable.
    pub fn name_in<'a>(&self, strings: &'a [u8]) -> &'a str {
        crate::cstr(strings, u64::from(self.name))
    }

    /// Where a defined, non-TLS symbol lies in the image `extent` describes,
    /// or `None`: undefined, thread-local, or a value outside the image.
    pub const fn address(&self, extent: Extent) -> Option<ImageOffset> {
        if !self.is_defined() || self.kind() == STT_TLS {
            return None;
        }
        extent.offset(self.value)
    }

    /// `S + A` for a symbol read as an offset into `segment` — psABI
    /// `st_value` for `STT_TLS` — or `None` when the sum leaves it.
    pub fn tls_offset(&self, addend: i64, segment: TlsSegment) -> Option<TlsOffset> {
        TlsOffset::of(self.value, addend, segment)
    }
}

/// `.dynsym` (or `.symtab`) paired with the string table its names live in.
///
/// `count` is the entries the *bytes* hold, never a number the file declared:
/// `sh_size / sh_entsize` and a `.gnu.hash` chain walk are both file-chosen,
/// and a symbol index is bounded by what can actually be read.
#[derive(Clone, Copy, Debug)]
pub struct SymTab<'a> {
    syms: &'a [u8],
    strs: &'a [u8],
}

impl<'a> SymTab<'a> {
    pub const fn new(syms: &'a [u8], strs: &'a [u8]) -> SymTab<'a> {
        SymTab { syms, strs }
    }

    /// An empty table, for a module that declares no `DT_SYMTAB`.
    ///
    /// Every lookup answers `None` and every index is out of range, which is
    /// what "no symbol table" means — the shape it replaces was an
    /// `expect("no dynsym")` reached only because four separate callers each
    /// happened to check something else first.
    pub const fn empty() -> SymTab<'static> {
        SymTab {
            syms: &[],
            strs: &[],
        }
    }

    pub const fn count(&self) -> usize {
        self.syms.len() / ENTRY_SIZE
    }

    pub const fn strings(&self) -> &'a [u8] {
        self.strs
    }

    pub fn get(&self, i: usize) -> Option<Sym> {
        if i >= self.count() {
            return None;
        }
        parse_at(self.syms, i.checked_mul(ENTRY_SIZE)?)
    }

    /// The `i`th symbol's name, or `""` for an index past the end or a
    /// `st_name` past the string table.
    pub fn name(&self, i: usize) -> &'a str {
        match self.get(i) {
            Some(sym) => sym.name_in(self.strs),
            None => "",
        }
    }

    /// The first defined symbol with this name, skipping index 0 (always the
    /// null entry).
    pub fn find(&self, name: &str) -> Option<(usize, Sym)> {
        (1..self.count()).find_map(|i| {
            let sym = self.get(i)?;
            (sym.is_defined() && self.name(i) == name).then_some((i, sym))
        })
    }

    /// The first defined `STT_TLS` symbol with this name.
    pub fn find_tls(&self, name: &str) -> Option<Sym> {
        (0..self.count()).find_map(|i| {
            let sym = self.get(i)?;
            (sym.is_defined() && sym.kind() == STT_TLS && self.name(i) == name).then_some(sym)
        })
    }

    /// Refuse a table any of whose defined symbols names nothing in its
    /// module: a value outside the image `extent`, or a `STT_TLS` one outside
    /// the module's `tls` segment (or in a module with none).
    ///
    /// A loader that has asked this once answers every later lookup through
    /// [`Sym::address`] and [`Sym::tls_offset`] from the same bytes, so none of
    /// those can come back empty for a symbol the table defines.
    pub fn bounded(&self, extent: Extent, tls: Option<TlsSegment>) -> Result<(), Error> {
        for (_, sym) in self.defined() {
            if sym.kind() == STT_TLS {
                tls.and_then(|segment| sym.tls_offset(0, segment))
                    .ok_or(Error::TlsSymbolOutsideSegment)?;
            } else {
                sym.address(extent).ok_or(Error::SymbolOutsideImage)?;
            }
        }
        Ok(())
    }

    /// The function containing `offset`, and how far into it that is.
    ///
    /// **This is what names a backtrace frame, and its caller is a panic
    /// handler** — so it allocates nothing, takes no lock, indexes nothing
    /// unchecked and cannot panic, exactly like every other view here. The
    /// cases it has to get right are in `tests/tables.rs`.
    ///
    /// `offset` is relative to the module's load base, because a symbol's
    /// `st_value` is. A caller holding an absolute address subtracts the base
    /// with `checked_sub` — an address below it belongs to no symbol here.
    ///
    /// Linear, because the table is not sorted and a panic handler may not
    /// build an index. The rules, each of which a case in that file pins:
    ///
    /// - `STT_FUNC` only, and never `st_value == 0` — a data object would
    ///   otherwise name a frame after a variable, and zero is what an undefined
    ///   symbol looks like;
    /// - the nearest symbol at or below `offset` wins, and between two at the
    ///   same address the one carrying a size does, because it is the only one
    ///   that can say whether the address is still inside it;
    /// - a symbol with `st_size` 0 bounds nothing and owns every address above
    ///   it until a later symbol takes over — hand-written assembly entry
    ///   points are that case;
    /// - past a sized symbol's last byte there is no answer. A return address
    ///   is the instruction after the `call`, so a call in tail position lands
    ///   one byte past its function and naming the *next* function would be a
    ///   lie about which one was executing;
    /// - a symbol whose name cannot be read is not an answer either: a frame
    ///   printing `+0x4` with nothing in front of it says less than the bare
    ///   address it replaces.
    pub fn resolve(&self, offset: u64) -> Option<(&'a str, u64)> {
        let mut best: Option<(usize, Sym)> = None;
        for i in 0..self.count() {
            let Some(sym) = self.get(i) else { continue };
            if sym.kind() != STT_FUNC || sym.value == 0 || sym.value > offset {
                continue;
            }
            let better = match best {
                None => true,
                Some((_, best)) => {
                    sym.value > best.value
                        || (sym.value == best.value && best.size == 0 && sym.size > 0)
                }
            };
            if better {
                best = Some((i, sym));
            }
        }
        let (index, sym) = best?;
        let within = offset.checked_sub(sym.value)?;
        if sym.size > 0 && within >= sym.size {
            return None;
        }
        let name = self.name(index);
        (!name.is_empty()).then_some((name, within))
    }

    /// Every index whose symbol is defined, in order.
    pub fn defined(self) -> impl Iterator<Item = (usize, Sym)> + 'a {
        (1..self.count()).filter_map(move |i| {
            let sym = self.get(i)?;
            sym.is_defined().then_some((i, sym))
        })
    }
}

/// One `Elf64_Sym` at a byte offset, for a caller holding a single record
/// rather than a table.
pub fn parse_at(data: &[u8], off: usize) -> Option<Sym> {
    if off >= data.len() {
        return None;
    }
    Some(Sym {
        name: read::u32_at(data, off)?,
        info: *data.get(off.checked_add(4)?)?,
        shndx: read::u16_at(data, off.checked_add(6)?)?,
        value: read::u64_at(data, off.checked_add(8)?)?,
        size: read::u64_at(data, off.checked_add(16)?)?,
    })
}
