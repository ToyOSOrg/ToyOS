//! One linker, two machines: the machine every input names is the machine the
//! output declares, and every number in the output that means something only
//! to one machine — `e_machine`, a dynamic relocation's type, a PE header's
//! machine — is that machine's. The outputs are read back by the `object`
//! crate, a reader this linker does not share code with.

mod common;

use common::{Case, ObjBuilder, RET};
use object::read::elf::{ElfFile64, FileHeader as _};
use object::read::pe::PeFile64;
use object::write::StandardSection;
use object::{elf, pe, Architecture, BinaryFormat, Object as _, ObjectSection as _, ObjectSymbol as _, RelocationFlags, SymbolScope};

const ADRP_X0: u32 = 0x9000_0000;
const ADD_X0_X0: u32 = 0x9100_0000;
const AARCH64_RET: u32 = 0xd65f_03c0;
const AARCH64_BL: u32 = 0x9400_0000;

fn words(insns: &[u32]) -> Vec<u8> {
    insns.iter().flat_map(|i| i.to_le_bytes()).collect()
}

/// `_start` takes `target`'s address PC-relatively, and `ptr` holds it as an
/// absolute pointer: one instruction pair the linker resolves and one word the
/// loader must rebase.
fn aarch64_elf_program() -> Vec<u8> {
    let mut b = ObjBuilder::for_machine(BinaryFormat::Elf, Architecture::Aarch64);
    let target = b.data("target", &[7u8; 8], SymbolScope::Linkage);
    let start = b.text("_start", &words(&[ADRP_X0, ADD_X0_X0, AARCH64_RET]), SymbolScope::Linkage);
    let at = b.symbol_offset(start);
    b.reloc(StandardSection::Text, at, target, 0, RelocationFlags::Elf { r_type: elf::R_AARCH64_ADR_PREL_PG_HI21 });
    b.reloc(StandardSection::Text, at + 4, target, 0, RelocationFlags::Elf { r_type: elf::R_AARCH64_ADD_ABS_LO12_NC });
    let ptr = b.data("ptr", &[0u8; 8], SymbolScope::Linkage);
    let ptr_at = b.symbol_offset(ptr);
    b.reloc(StandardSection::Data, ptr_at, target, 0, RelocationFlags::Elf { r_type: elf::R_AARCH64_ABS64 });
    b.finish()
}

fn symbol(file: &object::File, name: &str) -> u64 {
    file.symbols().find(|s| s.name() == Ok(name)).unwrap_or_else(|| panic!("no {name}")).address()
}

fn word_at(file: &object::File, vaddr: u64) -> u32 {
    use object::ObjectSegment as _;
    let seg = file.segments().find(|s| (s.address()..s.address() + s.size()).contains(&vaddr)).unwrap();
    let data = seg.data().unwrap();
    let off = (vaddr - seg.address()) as usize;
    u32::from_le_bytes(data[off..off + 4].try_into().unwrap())
}

#[test]
fn an_aarch64_pie_declares_aarch64_and_rebases_with_its_own_relative_type() {
    let out = Case::new("aarch64-pie").input("a.o", aarch64_elf_program()).link();

    let elf = ElfFile64::<object::Endianness>::parse(&*out).unwrap();
    assert_eq!(elf.elf_header().e_machine(object::Endianness::Little), elf::EM_AARCH64);

    let file = object::File::parse(&*out).unwrap();
    let (target, ptr, start) = (symbol(&file, "target"), symbol(&file, "ptr"), symbol(&file, "_start"));

    let relocs: Vec<_> = file.dynamic_relocations().expect("a PIE carries .rela.dyn").collect();
    assert_eq!(relocs.len(), 1, "{relocs:?}");
    let (offset, reloc) = &relocs[0];
    assert_eq!(*offset, ptr);
    assert_eq!(reloc.flags(), RelocationFlags::Elf { r_type: elf::R_AARCH64_RELATIVE });
    assert_eq!(reloc.addend(), target as i64);

    // ADRP names the page, the ADD the offset within it: together `target`.
    let adrp = word_at(&file, start);
    let imm = (((adrp >> 29) & 3) | (((adrp >> 5) & 0x7ffff) << 2)) as i64;
    let imm = (imm << 43) >> 43;
    let page = (start & !0xfff) as i64 + (imm << 12);
    let lo12 = ((word_at(&file, start + 4) >> 10) & 0xfff) as i64;
    assert_eq!((page + lo12) as u64, target);
}

#[test]
fn an_x86_64_pie_rebases_with_the_x86_64_relative_type() {
    let mut b = ObjBuilder::new();
    let target = b.data("target", &[7u8; 8], SymbolScope::Linkage);
    b.text("_start", &[RET], SymbolScope::Linkage);
    let ptr = b.data("ptr", &[0u8; 8], SymbolScope::Linkage);
    let ptr_at = b.symbol_offset(ptr);
    b.reloc(StandardSection::Data, ptr_at, target, 0, RelocationFlags::Elf { r_type: elf::R_X86_64_64 });
    let out = Case::new("x86-pie").input("a.o", b.finish()).link();

    let elf = ElfFile64::<object::Endianness>::parse(&*out).unwrap();
    assert_eq!(elf.elf_header().e_machine(object::Endianness::Little), elf::EM_X86_64);
    let file = object::File::parse(&*out).unwrap();
    let relocs: Vec<_> = file.dynamic_relocations().unwrap().collect();
    assert_eq!(relocs.len(), 1, "{relocs:?}");
    assert_eq!(relocs[0].1.flags(), RelocationFlags::Elf { r_type: elf::R_X86_64_RELATIVE });
    assert_eq!(relocs[0].1.addend(), symbol(&file, "target") as i64);
}

#[test]
fn inputs_for_two_machines_are_refused() {
    let mut x86 = ObjBuilder::new();
    x86.text("_start", &[RET], SymbolScope::Linkage);
    let mut arm = ObjBuilder::for_machine(BinaryFormat::Elf, Architecture::Aarch64);
    arm.text("helper", &words(&[AARCH64_RET]), SymbolScope::Linkage);
    let said = Case::new("mixed")
        .input("x86.o", x86.finish())
        .input("arm.o", arm.finish())
        .link_expecting_failure();
    assert!(said.contains("arm.o") && said.contains("Aarch64"), "{said}");
}

#[test]
fn an_aarch64_pe_declares_arm64_and_patches_its_calls() {
    let mut b = ObjBuilder::for_machine(BinaryFormat::Coff, Architecture::Aarch64);
    let helper = b.text("helper", &words(&[AARCH64_RET]), SymbolScope::Linkage);
    let target = b.data("target", &[7u8; 8], SymbolScope::Linkage);
    let start = b.text("efi_main", &words(&[AARCH64_BL, ADRP_X0, ADD_X0_X0, AARCH64_RET]), SymbolScope::Linkage);
    let at = b.symbol_offset(start);
    b.reloc(StandardSection::Text, at, helper, 0, RelocationFlags::Coff { typ: pe::IMAGE_REL_ARM64_BRANCH26 });
    b.reloc(StandardSection::Text, at + 4, target, 0, RelocationFlags::Coff { typ: pe::IMAGE_REL_ARM64_PAGEBASE_REL21 });
    b.reloc(StandardSection::Text, at + 8, target, 0, RelocationFlags::Coff { typ: pe::IMAGE_REL_ARM64_PAGEOFFSET_12A });
    let ptr = b.data("ptr", &[0u8; 8], SymbolScope::Linkage);
    let ptr_at = b.symbol_offset(ptr);
    b.reloc(StandardSection::Data, ptr_at, target, 0, RelocationFlags::Coff { typ: pe::IMAGE_REL_ARM64_ADDR64 });
    let out = Case::new("aarch64-pe").input("a.obj", b.finish()).arg("--pe").arg("-e").arg("efi_main").link();

    let file = PeFile64::parse(&*out).unwrap();
    assert_eq!(file.nt_headers().file_header.machine.get(object::LittleEndian), pe::IMAGE_FILE_MACHINE_ARM64);
    assert_eq!(file.architecture(), Architecture::Aarch64);

    // `helper` leads `.text` and `efi_main` follows it at the next 16 bytes;
    // `target` leads `.data` and `ptr` follows it.
    let file = object::File::parse(&*out).unwrap();
    let entry = file.entry();
    let text = file.section_by_name(".text").unwrap();
    let data = file.section_by_name(".data").unwrap();
    assert_eq!(entry, text.address() + 16);
    let bl = word_at(&file, entry);
    assert_eq!(bl & 0xfc00_0000, AARCH64_BL);
    assert_eq!((((bl & 0x03ff_ffff) as i64) << 38) >> 36, -16, "the call reaches helper");
    let adrp = word_at(&file, entry + 4);
    let imm = (((adrp >> 29) & 3) | (((adrp >> 5) & 0x7ffff) << 2)) as i64;
    let page = (entry as i64 + 4) & !0xfff;
    let lo12 = ((word_at(&file, entry + 8) >> 10) & 0xfff) as i64;
    assert_eq!((page + (((imm << 43) >> 43) << 12) + lo12) as u64, data.address());
    let bytes = data.data().unwrap();
    assert_eq!(u64::from_le_bytes(bytes[8..16].try_into().unwrap()), data.address(), "ptr holds target");
}
