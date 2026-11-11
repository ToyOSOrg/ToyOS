//! A section the build writes reads back as written, and every malformation
//! the reader fences is refused by name.

use toyos_abi::syscall::{SpawnMap, VmProt};
use toyos_kimage::{Input, Kernel, StackedImage, Symbol};

fn input() -> Input {
    Input {
        machine: 62,
        entry: 0x1010,
        memory: 0x4000,
        relro: (0x2000, 0x3000),
        ranges: vec![(0, 0x1000, VmProt::Read), (0x1000, 0x2000, VmProt::ReadExec), (0x2000, 0x4000, VmProt::ReadWrite)],
        relocations: vec![(0x2000, 0x1010), (0x2008, 0)],
        symbols: vec![
            Symbol { addr: 0x1000, size: 0x10, name: "a".into() },
            Symbol { addr: 0x1020, size: 0, name: "b".into() },
            Symbol { addr: 0x1080, size: 4, name: String::new() },
        ],
        supervisor_entry: 0x2_0000_1000,
        supervisor_base: 0x2_0000_0000,
        supervisor_build_id: vec![0x5a; 20],
        supervisor_maps: vec![SpawnMap { addr: 0x2_0000_0000, len: 0x1000, object: 0, offset: 0, copied: 0x800, prot: VmProt::ReadExec as u64 }],
        supervisor_len: 0x1234,
        image: vec![0xcc; 0x2100],
    }
}

#[test]
fn a_section_reads_back_as_written() {
    let bytes = input().write();
    let kernel = Kernel::parse(&bytes).unwrap();
    assert_eq!((kernel.machine, kernel.entry, kernel.memory, kernel.relro), (62, 0x1010, 0x4000, (0x2000, 0x3000)));
    assert_eq!(kernel.text().unwrap().lo, 0x1000);
    assert_eq!(kernel.relocations().collect::<Vec<_>>(), [(0x2000, 0x1010), (0x2008, 0)]);
    assert_eq!(kernel.image, &[0xcc; 0x2100][..]);
    assert_eq!((kernel.supervisor.entry, kernel.supervisor.build_id, kernel.supervisor.len), (0x2_0000_1000, &[0x5a; 20][..], 0x1234));
    assert_eq!(kernel.supervisor.maps().collect::<Vec<_>>(), input().supervisor_maps);
}

/// The rules `SymTab::resolve` names a frame by: nearest at or below, a sized
/// one owning only its bytes, one with no size owning everything to the next,
/// and an empty name naming nothing.
#[test]
fn a_symbol_resolves_as_the_elf_table_would() {
    let bytes = input().write();
    let symbols = Kernel::parse(&bytes).unwrap().symbols;
    assert_eq!(symbols.resolve(0xfff), None);
    assert_eq!(symbols.resolve(0x1004), Some(("a", 4)));
    assert_eq!(symbols.resolve(0x1010), None);
    assert_eq!(symbols.resolve(0x1070), Some(("b", 0x50)));
    assert_eq!(symbols.resolve(0x1081), None);
}

#[test]
fn a_malformed_section_is_refused_by_name() {
    let refused = |edit: &dyn Fn(&mut Input)| {
        let mut i = input();
        edit(&mut i);
        Kernel::parse(&i.write()).err()
    };
    assert!(refused(&|i| i.ranges.push((0x3000, 0x4000, VmProt::ReadExec))).is_some(), "two executable ranges");
    assert!(refused(&|i| i.ranges[2].1 = 0x5000).is_some(), "a range past memory");
    assert!(refused(&|i| i.relocations.push((0x3ffc, 0))).is_some(), "a relocation past memory");
    assert!(refused(&|i| i.symbols.swap(0, 1)).is_some(), "symbols out of order");
    assert!(refused(&|i| i.image = vec![0; 0x5000]).is_some(), "bytes past memory");
    let bytes = input().write();
    assert!(Kernel::parse(&bytes[..100]).is_err());
    assert!(Kernel::parse(b"TOYOSIMG").is_err());
}

#[test]
fn the_stack_after_an_image_is_aligned_past_a_whole_page_wherever_the_image_ends() {
    const STACK: u64 = 8 * 1024 * 1024;
    assert_eq!(StackedImage::place(0x41_0008, STACK), Some(StackedImage { size: 0xc1_2000, stack: 0x41_2000 }));
    for end in [1, 0x40_fc30, 0x41_0000, 0x41_0008, 0x41_0ff8, 0x41_0fff] {
        let placed = StackedImage::place(end, STACK).expect("fits");
        let guard = placed.stack - 4096;
        assert!(guard >= end && guard - end < 4096, "{end:#x}: {placed:?}");
        assert_eq!(placed.stack % 16, 0);
        assert_eq!(placed.size - placed.stack, STACK);
    }
    assert_eq!(StackedImage::place(u64::MAX - 8, STACK), None);
    assert_eq!(StackedImage::place(0x41_0000, STACK + 8), None);
}
