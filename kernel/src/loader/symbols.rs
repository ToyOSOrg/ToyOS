//! The executable's exported symbols, for binding a library's `GLOB_DAT` and
//! `JUMP_SLOT` slots against.
//!
//! Nothing here owns a string: the maps borrow the caller's tables and die
//! with the spawn that built them.

use alloc::vec::Vec;
use alloc::collections::BTreeMap;

use super::read_elf_table;
use crate::file_backing::FileBacking;
use crate::UserAddr;
use toyos_elf::section::{SectionTable, SHT_SYMTAB};
use toyos_elf::sym::{SymTab, STT_TLS};
use toyos_elf::{Error, Extent, Layout};

/// Every defined, named symbol in `.dynsym`, at its runtime address: no
/// binding filter, since being in `.dynsym` and defined is the export.
pub fn dynamic_map<'a>(
    symbols: &SymTab<'a>,
    image_start: UserAddr,
    extent: Extent,
) -> Result<BTreeMap<&'a str, UserAddr>, Error> {
    map(symbols, image_start, extent, |_| true)
}

/// The same over `.symtab`, which also holds locals no other module may
/// bind to.
pub fn static_map<'a>(
    symbols: &SymTab<'a>,
    image_start: UserAddr,
    extent: Extent,
) -> Result<BTreeMap<&'a str, UserAddr>, Error> {
    map(symbols, image_start, extent, |s: &toyos_elf::Sym| s.is_exported())
}

/// A thread-local symbol has no address to bind a slot to, so it is not in
/// the map; any other one whose value is outside the image refuses it.
fn map<'a>(
    symbols: &SymTab<'a>,
    image_start: UserAddr,
    extent: Extent,
    keep: impl Fn(&toyos_elf::Sym) -> bool,
) -> Result<BTreeMap<&'a str, UserAddr>, Error> {
    let mut map = BTreeMap::new();
    for (i, sym) in symbols.defined() {
        let name = symbols.name(i);
        if name.is_empty() || !keep(&sym) || sym.kind() == STT_TLS {
            continue;
        }
        let at = sym.address(extent).ok_or(Error::SymbolOutsideImage)?;
        map.insert(name, image_start + at.get());
    }
    Ok(map)
}

/// `.symtab` and its `.strtab`, read whole — the fallback for a PIE that
/// exports nothing through `.dynsym`.
pub fn read_symtab(backing: &dyn FileBacking, layout: &Layout) -> Option<(Vec<u8>, Vec<u8>)> {
    let table = layout.section_headers()?;
    let shdrs = super::read_file_range(backing, table.file_offset, table.byte_len());
    let (syms, strs) = SectionTable::new(&shdrs).symbols(SHT_SYMTAB)?;

    let (Some(sym_data), Some(str_data)) = (
        read_elf_table(backing, syms.offset, syms.size as usize),
        read_elf_table(backing, strs.offset, strs.size as usize),
    ) else {
        log!(
            "ELF: .symtab {} / .strtab {} exceed one kernel allocation, no symbol map",
            syms.size, strs.size
        );
        return None;
    };
    Some((sym_data, str_data))
}
