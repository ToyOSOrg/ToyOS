//! A program laid out for a spawn: against lld's own images, read by `object`
//! (not ToyOS code), and against the files a spawn must refuse.

#[path = "elf/common/mod.rs"]
#[allow(dead_code)]
mod common;

use common::*;
use object::{Object, ObjectSegment, SegmentFlags};
use toyos::elf::program::{lay_out, BASE};
use toyos::elf::Machine;
use toyos_abi::syscall::{SpawnMap, VmProt};
use toyos_abi::HANDLE_INVALID;

const OBJECT: u64 = 7;
const NONE: u64 = HANDLE_INVALID.0 as u64;

/// Every `PT_LOAD` of lld's image is one range at its page-rounded vaddr above
/// [`BASE`], copied from the object from its file page, with the protection
/// its flags give; the holes between them are reserved; the entry is the
/// file's, moved by the base.
#[test]
fn lld_images_are_laid_out_as_object_reads_them() {
    for (file, machine) in [
        (&include_bytes!("relocate/x86_64.elf")[..], Machine::X86_64),
        (&include_bytes!("relocate/aarch64.elf")[..], Machine::Aarch64),
    ] {
        let program = lay_out(file, machine, OBJECT, BASE).expect("lld's image lays out");
        let elf = object::File::parse(file).expect("object reads lld's image");
        assert_eq!(program.entry, BASE + elf.entry());
        assert_eq!(program.image_base, BASE);
        let loads: Vec<_> = program.maps().iter().filter(|m| m.object == OBJECT).copied().collect();
        let segments: Vec<_> = elf.segments().collect();
        assert_eq!(loads.len(), segments.len());
        for (map, segment) in loads.iter().zip(&segments) {
            let SegmentFlags::Elf { p_flags } = segment.flags() else { unreachable!("an ELF's") };
            let (offset, filesz) = segment.file_range();
            let lo = segment.address() & !0xfff;
            let hi = (segment.address() + segment.size()).next_multiple_of(0x1000);
            let prot = match p_flags & 3 {
                1 | 3 => VmProt::ReadExec,
                2 => VmProt::ReadWrite,
                _ => VmProt::Read,
            };
            let want = SpawnMap { addr: BASE + lo, len: hi - lo, object: OBJECT, offset: offset & !0xfff, copied: filesz + offset % 0x1000, prot: prot as u64 };
            assert_eq!(*map, want, "{machine:?}");
        }
        // No range meets another, and every hole between two is reserved.
        for pair in program.maps().windows(2) {
            assert_eq!(pair[0].addr + pair[0].len, pair[1].addr, "{machine:?}: a gap no range reserves");
        }
        for hole in program.maps().iter().filter(|m| m.object == NONE) {
            assert_eq!((hole.prot, hole.copied), (VmProt::None as u64, 0));
        }
    }
}

/// A spawn's base is where the lowest page goes, and the image base is that
/// less the image's own lowest address.
#[test]
fn the_image_moves_with_its_base() {
    let file = Elf::new(0x3000)
        .ph(Phdr::load(0, 0x10_0000, 0x1000, 0x1000, PF_R | PF_X))
        .ph(Phdr::load(0x1000, 0x10_1000, 0x800, 0x2000, PF_R | PF_W))
        .entry(0x10_0040)
        .build();
    let program = lay_out(&file, Machine::X86_64, OBJECT, BASE).unwrap();
    assert_eq!((program.image_base, program.entry), (BASE - 0x10_0000, BASE + 0x40));
    assert_eq!(
        program.maps(),
        [
            SpawnMap { addr: BASE, len: 0x1000, object: OBJECT, offset: 0, copied: 0x1000, prot: VmProt::ReadExec as u64 },
            SpawnMap { addr: BASE + 0x1000, len: 0x2000, object: OBJECT, offset: 0x1000, copied: 0x800, prot: VmProt::ReadWrite as u64 },
        ]
    );
}

/// A segment both writable and executable is mapped `ReadExec`: no range a
/// spawn asks for is both.
#[test]
fn a_writable_executable_segment_runs_but_is_not_written() {
    let file = Elf::new(0x2000).ph(Phdr::load(0, 0, 0x2000, 0x2000, PF_R | PF_W | PF_X)).build();
    assert_eq!(lay_out(&file, Machine::X86_64, OBJECT, BASE).unwrap().maps()[0].prot, VmProt::ReadExec as u64);
}

/// A segment's copy stops at the file's end: what the file does not hold is
/// zeros, never bytes past the object.
#[test]
fn a_copy_stops_at_the_files_end() {
    let file = Elf::new(0x1800).ph(Phdr::load(0, 0, 0x1800, 0x1800, PF_R | PF_X)).build();
    let map = lay_out(&file[..0x1200], Machine::X86_64, OBJECT, BASE);
    // The header page alone is held; the parse needs only it.
    assert_eq!(map.map(|p| p.maps()[0].copied), Ok(0x1200));
}

#[test]
fn what_a_spawn_cannot_start_is_refused_by_name() {
    // No `PT_LOAD` maps the program header table, which the program reads.
    let unmapped = Elf::new(0x2000).ph(Phdr::load(0x1000, 0x1000, 0x1000, 0x1000, PF_R | PF_X)).entry(0x1000).build();
    assert_eq!(lay_out(&unmapped, Machine::X86_64, OBJECT, BASE), Err("ELF: no PT_LOAD maps the program header table"));
    // Two segments on one page could not each have their protection.
    let shared = Elf::new(0x2000)
        .ph(Phdr::load(0, 0, 0x800, 0x800, PF_R | PF_X))
        .ph(Phdr::load(0x800, 0x800, 0x800, 0x800, PF_R | PF_W))
        .build();
    assert_eq!(lay_out(&shared, Machine::X86_64, OBJECT, BASE), Err("ELF: two PT_LOAD segments share a page"));
    // Another machine's image.
    assert!(lay_out(&Elf::honest(0x1000).build(), Machine::Aarch64, OBJECT, BASE).is_err());
    // A base that is no page boundary.
    assert!(lay_out(&Elf::honest(0x1000).build(), Machine::X86_64, OBJECT, BASE + 8).is_err());
}

/// A build-id note is carried, as the crash record names the program by it.
#[test]
fn the_build_id_is_carried() {
    let file = include_bytes!("elf/fixtures/input-test.bin");
    assert_eq!(lay_out(file, Machine::X86_64, OBJECT, BASE).unwrap().build_id(), toyos::elf::note::build_id_in(file).unwrap_or(&[]));
}
