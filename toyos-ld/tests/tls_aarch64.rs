//! AArch64 thread-local storage: TLS variant I and the TLS descriptor.
//!
//! The numbers asserted here come from the AArch64 ELF ABI, not from the
//! linker: variant I puts a two-word TCB at the thread pointer and the
//! executable's TLS block after it at the first offset the segment's alignment
//! allows, so a variable at offset `o` in a 64-byte-aligned segment is at
//! `align_up(16, 64) + o` from TPIDR_EL0; a descriptor sequence the executable
//! defines relaxes to `movz x0; movk x0; nop; nop` (local-exec), one another
//! module defines to `adrp x0; ldr x0; nop; nop` over a GOT slot the loader
//! fills with R_AARCH64_TLS_TPREL (initial-exec), and a shared library keeps
//! the sequence and asks the loader for the descriptor pair with
//! R_AARCH64_TLSDESC. The outputs are read back by the `object` crate.

mod common;

use common::{Case, ObjBuilder};
use object::write::StandardSection;
use object::{elf, Architecture, BinaryFormat, Object as _, ObjectSegment as _, ObjectSymbol as _, RelocationFlags, RelocationTarget, SymbolKind, SymbolScope};

const ADRP_X0: u32 = 0x9000_0000;
const LDR_X1_X0: u32 = 0xf940_0001;
const ADD_X0_X0: u32 = 0x9100_0000;
const BLR_X1: u32 = 0xd63f_0020;
const RET: u32 = 0xd65f_03c0;
const NOP: u32 = 0xd503_201f;
/// `add x0, x1, #0, lsl #12`, the thread pointer read into x1 before it.
const ADD_X0_X1_LSL12: u32 = 0x9140_0020;

fn words(insns: &[u32]) -> Vec<u8> {
    insns.iter().flat_map(|i| i.to_le_bytes()).collect()
}

/// `_start` (or `name`) running one TLS descriptor sequence for `target`.
fn descriptor_call(b: &mut ObjBuilder, name: &str, target: object::write::SymbolId) {
    let f = b.text(name, &words(&[ADRP_X0, LDR_X1_X0, ADD_X0_X0, BLR_X1, RET]), SymbolScope::Linkage);
    let at = b.symbol_offset(f);
    for (i, r_type) in [
        elf::R_AARCH64_TLSDESC_ADR_PAGE21,
        elf::R_AARCH64_TLSDESC_LD64_LO12,
        elf::R_AARCH64_TLSDESC_ADD_LO12,
        elf::R_AARCH64_TLSDESC_CALL,
    ]
    .into_iter()
    .enumerate()
    {
        b.reloc(StandardSection::Text, at + 4 * i as u64, target, 0, RelocationFlags::Elf { r_type });
    }
}

fn symbol(file: &object::File, name: &str) -> u64 {
    file.symbols().find(|s| s.name() == Ok(name)).unwrap_or_else(|| panic!("no {name}")).address()
}

fn word_at(file: &object::File, vaddr: u64) -> u32 {
    let seg = file.segments().find(|s| (s.address()..s.address() + s.size()).contains(&vaddr)).unwrap();
    let data = seg.data().unwrap();
    let off = (vaddr - seg.address()) as usize;
    u32::from_le_bytes(data[off..off + 4].try_into().unwrap())
}

fn words_at(file: &object::File, vaddr: u64, n: usize) -> Vec<u32> {
    (0..n).map(|i| word_at(file, vaddr + 4 * i as u64)).collect()
}

/// The address an `adrp` at `pc` names.
fn adrp_target(adrp: u32, pc: u64) -> u64 {
    let imm = (((adrp >> 29) & 3) | (((adrp >> 5) & 0x7ffff) << 2)) as i64;
    let imm = (imm << 43) >> 43;
    ((pc & !0xfff) as i64 + (imm << 12)) as u64
}

fn pt_tls(out: &[u8]) -> (u64, u64) {
    use object::read::elf::{ElfFile64, ProgramHeader as _};
    let elf = ElfFile64::<object::Endianness>::parse(out).unwrap();
    let e = object::Endianness::Little;
    let tls = elf
        .elf_program_headers()
        .iter()
        .find(|p| p.p_type(e) == elf::PT_TLS)
        .expect("a PT_TLS");
    (tls.p_vaddr(e), tls.p_align(e))
}

/// A module defining two thread-locals, `first` (8 bytes) and `v` after it.
fn two_locals(b: &mut ObjBuilder, scope: SymbolScope) -> object::write::SymbolId {
    b.tls("first", &[1u8; 8], scope);
    b.tls("v", &[2u8; 8], scope)
}

#[test]
fn an_executable_relaxes_its_own_descriptor_to_the_variant_one_offset() {
    let mut b = ObjBuilder::for_machine(BinaryFormat::Elf, Architecture::Aarch64);
    let v = two_locals(&mut b, SymbolScope::Linkage);
    descriptor_call(&mut b, "_start", v);
    // Local-exec written by the compiler: the same variable, the same offset.
    let le = b.text("le", &words(&[ADD_X0_X1_LSL12, ADD_X0_X0, RET]), SymbolScope::Linkage);
    let le_at = b.symbol_offset(le);
    b.reloc(StandardSection::Text, le_at, v, 0, RelocationFlags::Elf { r_type: elf::R_AARCH64_TLSLE_ADD_TPREL_HI12 });
    b.reloc(StandardSection::Text, le_at + 4, v, 0, RelocationFlags::Elf { r_type: elf::R_AARCH64_TLSLE_ADD_TPREL_LO12_NC });
    let out = Case::new("a64-tls-le").input("a.o", b.finish()).link();

    let file = object::File::parse(&*out).unwrap();
    let (tls_vaddr, tls_align) = pt_tls(&out);
    assert_eq!(tls_align, 64);
    assert_eq!(tls_vaddr % 64, 0, "the TLS segment starts off its alignment");
    // `v` is 8 bytes into the block, and the block 64 bytes past TP.
    let tp_offset: u32 = 64 + 8;
    let start = symbol(&file, "_start");
    assert_eq!(
        words_at(&file, start, 5),
        [0xd2a0_0000 | ((tp_offset >> 16) << 5), 0xf280_0000 | ((tp_offset & 0xffff) << 5), NOP, NOP, RET],
        "movz x0, #hi, lsl 16; movk x0, #lo; nop; nop"
    );
    let le = symbol(&file, "le");
    let [hi, lo] = [word_at(&file, le), word_at(&file, le + 4)];
    assert_eq!(((hi >> 10) & 0xfff) << 12 | ((lo >> 10) & 0xfff), tp_offset);
    let dynamic = file.dynamic_relocations().map(|r| r.count()).unwrap_or(0);
    assert_eq!(dynamic, 0, "a relaxed descriptor asked the loader for something");
}

#[test]
fn a_shared_library_keeps_the_descriptor_and_asks_the_loader_for_its_pair() {
    let mut b = ObjBuilder::for_machine(BinaryFormat::Elf, Architecture::Aarch64);
    let v = two_locals(&mut b, SymbolScope::Dynamic);
    descriptor_call(&mut b, "reader", v);
    let out = Case::new("a64-tls-so").arg("-shared").input("a.o", b.finish()).link();

    let file = object::File::parse(&*out).unwrap();
    let relocs: Vec<_> = file.dynamic_relocations().expect("a .rela.dyn").collect();
    let descs: Vec<_> = relocs
        .iter()
        .filter(|(_, r)| r.flags() == RelocationFlags::Elf { r_type: elf::R_AARCH64_TLSDESC })
        .collect();
    assert_eq!(descs.len(), 1, "{relocs:?}");
    let (pair, reloc) = descs[0];
    assert_eq!(reloc.addend(), 8, "the descriptor's argument is `v`'s offset in the module's block");
    assert_eq!(reloc.target(), RelocationTarget::Absolute, "a module's own variable is named by offset");
    assert_eq!(pair % 8, 0);

    let reader = symbol(&file, "reader");
    let seq = words_at(&file, reader, 5);
    assert_eq!(adrp_target(seq[0], reader), pair & !0xfff);
    assert_eq!(seq[1] & !(0xfff << 10), LDR_X1_X0);
    assert_eq!(((seq[1] >> 10) & 0xfff) * 8, (*pair & 0xfff) as u32, "the ldr reads the pair's first word");
    assert_eq!(seq[2] & !(0xfff << 10), ADD_X0_X0);
    assert_eq!(((seq[2] >> 10) & 0xfff) as u64, pair & 0xfff, "x0 is the pair's address");
    assert_eq!(&seq[3..], [BLR_X1, RET], "the call to the resolver is kept");
}

#[test]
fn an_executable_reaches_another_module_s_variable_through_a_tp_offset_slot() {
    let mut lib = ObjBuilder::for_machine(BinaryFormat::Elf, Architecture::Aarch64);
    two_locals(&mut lib, SymbolScope::Dynamic);
    lib.text("helper", &words(&[RET]), SymbolScope::Dynamic);
    let so = Case::new("a64-tls-lib").arg("-shared").input("a.o", lib.finish()).link();

    let mut b = ObjBuilder::for_machine(BinaryFormat::Elf, Architecture::Aarch64);
    let v = b.undefined("v", SymbolKind::Tls);
    descriptor_call(&mut b, "_start", v);
    let out = Case::new("a64-tls-ie").input("a.o", b.finish()).input("libv.so", so).link();

    let file = object::File::parse(&*out).unwrap();
    let relocs: Vec<_> = file.dynamic_relocations().expect("a .rela.dyn").collect();
    let tprel: Vec<_> = relocs
        .iter()
        .filter(|(_, r)| r.flags() == RelocationFlags::Elf { r_type: elf::R_AARCH64_TLS_TPREL })
        .collect();
    assert_eq!(tprel.len(), 1, "{relocs:?}");
    let (slot, reloc) = tprel[0];
    assert!(matches!(reloc.target(), RelocationTarget::Symbol(_)), "the slot names `v` for the loader");

    let start = symbol(&file, "_start");
    let seq = words_at(&file, start, 5);
    assert_eq!(adrp_target(seq[0], start), slot & !0xfff);
    assert_eq!(seq[1], 0xf940_0000 | ((((*slot & 0xfff) >> 3) as u32) << 10), "ldr x0, [x0, #slot]");
    assert_eq!(&seq[2..], [NOP, NOP, RET]);
}
