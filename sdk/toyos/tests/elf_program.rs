//! A program laid out for a spawn: against lld's own images, read by `object`
//! (not ToyOS code), and against the files a spawn must refuse.

#[path = "elf/common/mod.rs"]
#[allow(dead_code)]
mod common;

use common::*;
use object::{Object, ObjectSegment, SegmentFlags};
use toyos::elf::program::{lay_out, Source, BASE};
use toyos::elf::Machine;
use toyos::startup::AT;
use toyos_abi::syscall::{SpawnBacking, SpawnMap, VmProt};
use toyos_abi::HANDLE_INVALID;

const OBJECT: u64 = 7;
/// The object as one its holder may write, every page of which is copied.
const COPIED: Source = Source::Copied(OBJECT);
const SHARE: u64 = SpawnBacking::Share as u64;
const NONE: u64 = HANDLE_INVALID.0 as u64;

/// Every `PT_LOAD` of lld's image is one range at its page-rounded vaddr above
/// [`BASE`], out of the object from its file page, with the protection its
/// flags give: copied from an object its holder may write, and from one
/// nobody can write the object's own pages where the program cannot write
/// them. The holes between them are reserved; the entry is the file's, moved
/// by the base.
#[test]
fn lld_images_are_laid_out_as_object_reads_them() {
    for (file, machine) in [
        (&include_bytes!("relocate/x86_64.elf")[..], Machine::X86_64),
        (&include_bytes!("relocate/aarch64.elf")[..], Machine::Aarch64),
    ] {
        for source in [COPIED, Source::Immutable(OBJECT)] {
            let program = lay_out(file, machine, source).expect("lld's image lays out");
            let elf = object::File::parse(file).expect("object reads lld's image");
            assert_eq!(program.entry, BASE + elf.entry());
            assert_eq!(program.image_base, BASE);
            let loads: Vec<_> = program.maps().iter().filter(|m| m.object == OBJECT).copied().collect();
            let segments: Vec<_> = elf.segments().collect();
            assert_eq!(loads.len(), segments.len());
            let mut shared = 0;
            for (map, segment) in loads.iter().zip(&segments) {
                let SegmentFlags::Elf { p_flags } = segment.flags() else { unreachable!("an ELF's") };
                let (offset, filesz) = segment.file_range();
                let lo = segment.address() & !0xfff;
                let hi = (segment.address() + segment.size()).next_multiple_of(0x1000);
                let prot = match p_flags & 3 {
                    1 => VmProt::ReadExec,
                    2 => VmProt::ReadWrite,
                    _ => VmProt::Read,
                };
                // lld's read-only segments end with their file bytes.
                let share = source != COPIED && prot != VmProt::ReadWrite;
                assert!(!share || filesz == segment.size(), "{machine:?}: a read-only segment with zeros past its file bytes");
                shared += usize::from(share);
                let (copied, backing) = if share { (0, SHARE) } else { (filesz + offset % 0x1000, 0) };
                let want = SpawnMap { addr: BASE + lo, len: hi - lo, object: OBJECT, offset: offset & !0xfff, copied, prot: prot as u64, backing };
                assert_eq!(*map, want, "{machine:?} {source:?}");
            }
            assert_eq!(shared > 0, source != COPIED, "{machine:?}: lld's image has segments its program cannot write");
            // No range meets another, and every hole between two is reserved.
            for pair in program.maps().windows(2) {
                assert_eq!(pair[0].addr + pair[0].len, pair[1].addr, "{machine:?}: a gap no range reserves");
            }
            for hole in program.maps()[1..].iter().filter(|m| m.object == NONE) {
                assert_eq!((hole.prot, hole.copied, hole.backing), (VmProt::None as u64, 0, 0));
            }
        }
    }
}

/// From an object nobody can write, a segment is still a copy where the
/// program may write it, where zeros follow its file bytes, where the file is
/// too short to hold it, and where it does not lie in its pages as it lies in
/// the file's: a copy alone holds those.
#[test]
fn a_segment_only_a_copy_holds_is_copied_from_an_immutable_object() {
    let backing = |file: &[u8], held: usize| {
        let program = lay_out(&file[..held], Machine::X86_64, Source::Immutable(OBJECT)).unwrap();
        program.maps()[2..].iter().filter(|m| m.object == OBJECT).map(|m| (m.backing, m.copied)).collect::<Vec<_>>()
    };
    let text = |filesz, memsz, flags| Elf::new(0x3000).ph(Phdr::load(0, 0, filesz, memsz, flags)).build();
    assert_eq!(backing(&text(0x1800, 0x1800, PF_R | PF_X), 0x3000), [(SHARE, 0)]);
    assert_eq!(backing(&text(0x1800, 0x1800, PF_R), 0x3000), [(SHARE, 0)]);
    assert_eq!(backing(&text(0x1800, 0x1800, PF_R | PF_W), 0x3000), [(0, 0x1800)]);
    assert_eq!(backing(&text(0x1800, 0x2000, PF_R | PF_X), 0x3000), [(0, 0x1800)]);
    assert_eq!(backing(&text(0x1800, 0x1800, PF_R | PF_X), 0x1200), [(0, 0x1200)]);
    let shifted = Elf::new(0x3000)
        .ph(Phdr::load(0, 0, 0x1000, 0x1000, PF_R | PF_X))
        .ph(Phdr::load(0x1800, 0x2000, 0x800, 0x800, PF_R))
        .build();
    assert_eq!(backing(&shifted, 0x3000), [(SHARE, 0), (0, 0x1000)]);
}

/// The lowest page goes at the base, the image base is that less the image's
/// own lowest address, and below the program lies a startup block that says
/// nothing: a page of zeros at its address, and reserved pages up to the base.
#[test]
fn the_image_lies_at_the_base_above_an_empty_startup_block() {
    let file = Elf::new(0x3000)
        .ph(Phdr::load(0, 0x10_0000, 0x1000, 0x1000, PF_R | PF_X))
        .ph(Phdr::load(0x1000, 0x10_1000, 0x800, 0x2000, PF_R | PF_W))
        .entry(0x10_0040)
        .build();
    let program = lay_out(&file, Machine::X86_64, COPIED).unwrap();
    assert_eq!((program.image_base, program.entry), (BASE - 0x10_0000, BASE + 0x40));
    assert_eq!(
        program.maps(),
        [
            SpawnMap { addr: AT, len: 0x1000, object: NONE, offset: 0, copied: 0, prot: VmProt::ReadWrite as u64, backing: 0 },
            SpawnMap { addr: AT + 0x1000, len: BASE - AT - 0x1000, object: NONE, offset: 0, copied: 0, prot: VmProt::None as u64, backing: 0 },
            SpawnMap { addr: BASE, len: 0x1000, object: OBJECT, offset: 0, copied: 0x1000, prot: VmProt::ReadExec as u64, backing: 0 },
            SpawnMap { addr: BASE + 0x1000, len: 0x2000, object: OBJECT, offset: 0x1000, copied: 0x800, prot: VmProt::ReadWrite as u64, backing: 0 },
        ]
    );
}

/// A segment both writable and executable is refused by name, as the loader
/// refuses a library's: no page is both.
#[test]
fn a_writable_executable_segment_is_refused() {
    let file = Elf::new(0x2000).ph(Phdr::load(0, 0, 0x2000, 0x2000, PF_R | PF_W | PF_X)).build();
    assert_eq!(lay_out(&file, Machine::X86_64, COPIED), Err("ELF: a PT_LOAD is both writable and executable"));
}

/// A segment's copy stops at the file's end: what the file does not hold is
/// zeros, never bytes past the object.
#[test]
fn a_copy_stops_at_the_files_end() {
    let file = Elf::new(0x1800).ph(Phdr::load(0, 0, 0x1800, 0x1800, PF_R | PF_X)).build();
    let map = lay_out(&file[..0x1200], Machine::X86_64, COPIED);
    // The header page alone is held; the parse needs only it.
    assert_eq!(map.map(|p| p.maps()[2].copied), Ok(0x1200));
}

#[test]
fn what_a_spawn_cannot_start_is_refused_by_name() {
    // No `PT_LOAD` maps the program header table, which the program reads.
    let unmapped = Elf::new(0x2000).ph(Phdr::load(0x1000, 0x1000, 0x1000, 0x1000, PF_R | PF_X)).entry(0x1000).build();
    assert_eq!(lay_out(&unmapped, Machine::X86_64, COPIED), Err("ELF: no PT_LOAD maps the program header table"));
    // Two segments on one page could not each have their protection.
    let shared = Elf::new(0x2000)
        .ph(Phdr::load(0, 0, 0x800, 0x800, PF_R | PF_X))
        .ph(Phdr::load(0x800, 0x800, 0x800, 0x800, PF_R | PF_W))
        .build();
    assert_eq!(lay_out(&shared, Machine::X86_64, COPIED), Err("ELF: two PT_LOAD segments share a page"));
    // Another machine's image.
    assert!(lay_out(&Elf::honest(0x1000).build(), Machine::Aarch64, COPIED).is_err());
}

/// A build-id note is carried, as the crash record names the program by it.
#[test]
fn the_build_id_is_carried() {
    let file = include_bytes!("elf/fixtures/input-test.bin");
    assert_eq!(lay_out(file, Machine::X86_64, COPIED).unwrap().build_id(), toyos::elf::note::build_id_in(file).unwrap_or(&[]));
}
