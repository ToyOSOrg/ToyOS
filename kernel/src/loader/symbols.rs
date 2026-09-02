//! The executable's exported symbols, for binding a library's `GLOB_DAT` and
//! `JUMP_SLOT` slots against.
//!
//! Nothing here owns a string: the maps borrow the caller's tables and die
//! with the spawn that built them.

use alloc::vec::Vec;
use alloc::collections::BTreeMap;

use super::read_elf_table;
use crate::file_backing::FileBacking;
use crate::mm::pmm::Category;
use crate::process::PageAlloc;
use crate::symbols::SymbolTable;
use crate::UserAddr;
use toyos_elf::section::{SectionTable, SHT_SYMTAB};
use toyos_elf::sym::SymTab;
use toyos_elf::Layout;

/// Every defined, named symbol in `.dynsym`, at its runtime address: no
/// binding filter, since being in `.dynsym` and defined is the export.
pub fn dynamic_map<'a>(symbols: &SymTab<'a>, base: UserAddr) -> BTreeMap<&'a str, UserAddr> {
    map(symbols, base, |_| true)
}

/// The same over `.symtab`, which also holds locals no other module may
/// bind to.
pub fn static_map<'a>(symbols: &SymTab<'a>, base: UserAddr) -> BTreeMap<&'a str, UserAddr> {
    map(symbols, base, |s: &toyos_elf::Sym| s.is_exported())
}

fn map<'a>(
    symbols: &SymTab<'a>,
    base: UserAddr,
    keep: impl Fn(&toyos_elf::Sym) -> bool,
) -> BTreeMap<&'a str, UserAddr> {
    let mut map = BTreeMap::new();
    for (i, sym) in symbols.defined() {
        let name = symbols.name(i);
        if !name.is_empty() && keep(&sym) {
            map.insert(name, base + sym.value);
        }
    }
    map
}

/// `.symtab` and its `.strtab`, read whole — the fallback for a PIE that
/// exports nothing through `.dynsym`.
pub fn read_symtab(backing: &dyn FileBacking, layout: &Layout) -> Option<(Vec<u8>, Vec<u8>)> {
    let table = layout.section_headers?;
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

/// Bound on symbol-table bytes the kernel holds per binary.
pub const MAX_SYMBOL_BYTES: usize = 16 * 1024 * 1024;

/// The table a process's backtraces are named from, read off its own file.
// Eight arguments: four are the two address ranges a backtrace checks against.
#[allow(clippy::too_many_arguments)]
pub fn read_backtrace_table(
    backing: &dyn FileBacking,
    layout: &Layout,
    path: &str,
    base: u64,
    prog_base: u64, prog_end: u64,
    stack_base: u64, stack_end: u64,
) -> SymbolTable {
    let empty = || SymbolTable::empty_with_bounds(prog_base, prog_end, stack_base, stack_end);

    let Some(table) = layout.section_headers else { return empty() };
    let shdrs = super::read_file_range(backing, table.file_offset, table.byte_len());
    let Some((syms, strs)) = SectionTable::new(&shdrs).symbols(SHT_SYMTAB) else {
        return empty();
    };

    // Bounded against the file: an unchecked section past EOF would read as
    // zeros, a silent null table.
    let file_size = backing.file_size();
    let fits = |sh: &toyos_elf::section::SectionHeader| {
        sh.offset.checked_add(sh.size).is_some_and(|end| end <= file_size)
    };
    if syms.size == 0 || !fits(&syms) || !fits(&strs) {
        return empty();
    }

    let total = (syms.size + strs.size) as usize;
    if total > MAX_SYMBOL_BYTES {
        log!(
            "ELF: {}: {} bytes of symbol table past the {} byte bound — its backtraces will be \
             bare addresses",
            path, total, MAX_SYMBOL_BYTES
        );
        return empty();
    }

    let Some(pages) = PageAlloc::new(total, Category::Elf) else {
        log!("ELF: {}: no {} bytes for its symbol table", path, total);
        return empty();
    };
    // `subslice` bounds each read against what `pages` returned, not a
    // recomputed sum.
    let dst = pages.window();
    if crate::elf::read_backing_into(backing, syms.offset, dst.subslice(0, syms.size as usize))
        .is_err()
        || crate::elf::read_backing_into(
            backing,
            strs.offset,
            dst.subslice(syms.size as usize, strs.size as usize),
        )
        .is_err()
    {
        log!("ELF: {}: its symbol table could not be read off the device", path);
        return empty();
    }

    SymbolTable::from_pages(
        pages,
        syms.size as usize,
        strs.size as usize,
        base,
        prog_base, prog_end, stack_base, stack_end,
    )
}
