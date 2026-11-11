//! The kernel section of a signed image (`toyos_kimage`), out of the kernel's
//! and the supervisor's ELF: what the loader and the kernel would otherwise
//! each parse at boot, read here once with the one parser (`toyos::elf`).
//! The kernel is ours and linked by this build, so anything here that it
//! could not be is this build's bug, and panics naming it.

use toyos::elf::program::{lay_out, Source};
use toyos::elf::rela::{self, Op, RelaTable, Rules};
use toyos::elf::section::{symbol_tables, SectionTable};
use toyos::elf::sym::{SymTab, STT_FUNC};
use toyos::elf::{file_range, Layout, Machine};
use toyos_abi::syscall::VmProt;
use toyos_kimage::{Input, Symbol, PAGE, SUPERVISOR_OBJECT};

use crate::arch::Arch;

fn machine(arch: Arch) -> (Machine, u16) {
    match arch {
        Arch::X86_64 => (Machine::X86_64, toyos_kimage::EM_X86_64),
        Arch::Aarch64 => (Machine::Aarch64, toyos_kimage::EM_AARCH64),
    }
}

/// The section of `kernel` and the supervisor `supervisor`, both for `arch`.
pub fn section(kernel: &[u8], supervisor: &[u8], arch: Arch) -> Vec<u8> {
    let (machine, e_machine) = machine(arch);
    let layout = Layout::parse(kernel, machine).unwrap_or_else(|e| panic!("the kernel's ELF: {e}"));
    let extent = layout.extent();
    // The image sits at its own addresses: an image offset is moved by the
    // extent's start, which the kernel links at a page boundary.
    let min = extent.min();
    assert!(min.is_multiple_of(PAGE), "the kernel's ELF: its lowest address {min:#x} is no page boundary");
    if let Some((a, b)) = layout.overlapping_load_pages(PAGE) {
        panic!("the kernel's ELF: PT_LOAD segments {a} and {b} share a page, which cannot have both their protections");
    }
    let memory = extent.max().next_multiple_of(PAGE);
    let mut image = Vec::new();
    let mut ranges = Vec::new();
    for segment in layout.segments() {
        let bytes = file_range(kernel, segment.file_offset(), segment.filesz())
            .expect("the kernel's ELF: a PT_LOAD's file bytes are past the file's end");
        let at = (min + segment.image().start().get()) as usize;
        if image.len() < at + bytes.len() {
            image.resize(at + bytes.len(), 0);
        }
        image[at..at + bytes.len()].copy_from_slice(bytes);
        let (lo, hi) = segment.page_range(PAGE);
        let flags = segment.flags();
        assert!(!(flags.executable() && flags.writable()), "the kernel's ELF: a segment both writable and executable");
        let prot = if flags.executable() {
            VmProt::ReadExec
        } else if flags.writable() {
            VmProt::ReadWrite
        } else {
            VmProt::Read
        };
        ranges.push((min + lo, min + hi, prot));
    }
    ranges.sort_unstable_by_key(|&(lo, ..)| lo);
    let relro = layout.relro_pages(PAGE).map_or((0, 0), |(lo, hi)| (min + lo, min + hi));

    // Every relocation is a `RELATIVE`: the loader writes its base plus the
    // target's address, and nothing else, before ExitBootServices.
    let sections = layout
        .section_headers()
        .and_then(|table| file_range(kernel, table.file_offset, table.byte_len() as u64))
        .map(SectionTable::new)
        .expect("the kernel's ELF: no section header table inside the file");
    let rela_sections = sections.rela_sections().unwrap_or_else(|form| panic!("the kernel's ELF: {form} is not supported"));
    let rules = Rules { extent, window: (0, memory), tls: None };
    let mut relocations = Vec::new();
    for section in rela_sections {
        let table = file_range(kernel, section.offset, section.size).expect("the kernel's ELF: SHT_RELA section is past the file's end");
        for raw in RelaTable::new(table, machine).iter() {
            match rela::parse(raw, &rules, SymTab::empty()).unwrap_or_else(|e| panic!("the kernel's ELF: {e}")).map(|r| (r.offset(), r.op())) {
                Some((offset, Op::Relative(target))) => relocations.push((offset, min + target.get())),
                None => {}
                Some(other) => panic!("the kernel's ELF: a relocation the loader does not apply: {other:?}"),
            }
        }
    }

    let program = lay_out(supervisor, machine, Source::Immutable(SUPERVISOR_OBJECT)).unwrap_or_else(|why| panic!("supervisor: {why}"));
    Input {
        machine: e_machine,
        entry: min + layout.entry().get(),
        memory,
        relro,
        ranges,
        relocations,
        symbols: symbols(kernel),
        supervisor_entry: program.entry,
        supervisor_base: program.image_base,
        supervisor_build_id: program.build_id().to_vec(),
        supervisor_maps: program.maps().to_vec(),
        supervisor_len: supervisor.len() as u64,
        image,
    }
    .write()
}

/// Each address a function of `kernel` begins at, ascending, and the function
/// `SymTab::resolve` names there: `STT_FUNC`, never at 0, and of two at one
/// address the one carrying a size.
fn symbols(kernel: &[u8]) -> Vec<Symbol> {
    symbol_tables(kernel).map_or_else(Vec::new, |(symtab, strtab)| chosen(SymTab::new(symtab, strtab)))
}

/// [`symbols`] of one table.
fn chosen(table: SymTab<'_>) -> Vec<Symbol> {
    let mut chosen: std::collections::BTreeMap<u64, (u64, usize)> = std::collections::BTreeMap::new();
    for i in 0..table.count() {
        let Some(sym) = table.get(i) else { continue };
        if sym.kind() != STT_FUNC || sym.value() == 0 {
            continue;
        }
        let best = chosen.entry(sym.value()).or_insert((sym.size(), i));
        if best.0 == 0 && sym.size() > 0 {
            *best = (sym.size(), i);
        }
    }
    chosen.into_iter().map(|(addr, (size, i))| Symbol { addr, size, name: table.name(i).to_string() }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The table names every address what the ELF symbol table names it,
    /// over a real binary: a frame the kernel prints from the table is the one
    /// it printed from its ELF.
    #[test]
    fn the_table_names_what_the_elf_table_names() {
        let file = include_bytes!("../sdk/toyos/tests/elf/fixtures/input-test.bin");
        let input = Input { symbols: symbols(file), memory: 0x10_0000, ranges: vec![(0, 0x1000, VmProt::ReadExec)], ..Input::default() };
        let bytes = input.write();
        let table = toyos_kimage::Kernel::parse(&bytes).expect("the section reads back").symbols;
        let (symtab, strtab) = symbol_tables(file).unwrap();
        let elf = SymTab::new(symtab, strtab);
        let mut checked = 0;
        for offset in (0..0x6_0000).step_by(3) {
            assert_eq!(table.resolve(offset), elf.resolve(offset), "{offset:#x}");
            checked += u32::from(elf.resolve(offset).is_some());
        }
        assert!(checked > 10_000, "only {checked} named offsets");
    }

    /// Of two functions at one address, the one with a size names it, before
    /// or after the one without; an object, or a function at 0, names nothing.
    #[test]
    fn of_two_at_one_address_the_sized_one_is_kept() {
        let entry = |name: u32, info: u8, value: u64, size: u64| {
            let mut e = vec![0u8; 24];
            e[0..4].copy_from_slice(&name.to_le_bytes());
            e[4] = info;
            e[6..8].copy_from_slice(&1u16.to_le_bytes());
            e[8..16].copy_from_slice(&value.to_le_bytes());
            e[16..24].copy_from_slice(&size.to_le_bytes());
            e
        };
        const FUNC: u8 = (1 << 4) | 2;
        const OBJECT: u8 = (1 << 4) | 1;
        let strs = b"\0bare\0sized\0data\0zero\0";
        let syms = [entry(0, 0, 0, 0), entry(1, FUNC, 0x100, 0), entry(6, FUNC, 0x100, 8), entry(12, OBJECT, 0x200, 8), entry(17, FUNC, 0, 4)].concat();
        let kept = chosen(SymTab::new(&syms, strs));
        assert_eq!(kept, [Symbol { addr: 0x100, size: 8, name: "sized".into() }]);
        let swapped = [entry(0, 0, 0, 0), entry(6, FUNC, 0x100, 8), entry(1, FUNC, 0x100, 0)].concat();
        assert_eq!(chosen(SymTab::new(&swapped, strs)), kept);
    }
}
