//! The positive control: the parser agrees with a real artifact.
//!
//! Every other test in this crate hands the parser bytes no linker would emit,
//! and a parser that refused everything would pass all of them. The fixture is
//! the first 4 KiB of `/system/bin/shell` as rust-lld linked it with
//! `--build-id` — which is all [`Layout::parse`] ever reads — so this is the
//! shape the loader actually meets at every boot.
//!
//! Refresh it with `dd if=userland/target/x86_64-unknown-toyos/toyos/shell
//! of=toyos-elf/tests/fixtures/lld-headers.bin bs=4096 count=1`, and expect
//! every number below to move: each is `readelf -hlnW` of the fixture.

use toyos_elf::{Layout, Machine};

const LLD_HEADERS: &[u8] = include_bytes!("fixtures/lld-headers.bin");

#[test]
fn a_linked_binary_parses_to_what_readelf_says() {
    let layout = Layout::parse(LLD_HEADERS, Machine::X86_64).expect("rust-lld's own output");

    assert_eq!(layout.entry().get(), 0x4a07c);
    assert_eq!(layout.extent().min(), 0);
    assert_eq!(layout.extent().max(), 0xc53d4);

    // R--, R-X, the RELRO data carrying TLS and `.dynamic` (RW-), and the
    // rest of the data with `.bss` (RW-).
    let segs = layout.segments();
    assert_eq!(segs.len(), 4);
    assert!(!segs[0].writable() && !segs[0].flags().executable());
    assert!(segs[1].flags().executable() && !segs[1].writable());
    assert!(segs[2].writable() && !segs[2].flags().executable());
    assert!(segs[3].writable() && !segs[3].flags().executable());
    assert_eq!(layout.writable_window(), Some((0xb9270, 0xc53d4)));

    let dynamic = layout.dynamic().map(|d| (d.file_offset(), d.image().start().get(), d.image().len()));
    assert_eq!(dynamic, Some((0xbe620, 0xc0620, 0x100)));
    let eh = layout.eh_frame_hdr().map(|r| (r.start().get(), r.len()));
    assert_eq!(eh, Some((0x2538c, 0x251c)));
    assert_eq!(layout.tls().unwrap().memsz(), 0x68);
    assert_eq!(layout.tls().unwrap().align(), 0x8);

    // `e_phoff` 0x40, eleven headers of 56 bytes, inside the first segment's
    // file bytes at offset 0.
    let table = layout.program_headers().map(|t| (t.image().start().get(), t.image().len(), t.count()));
    assert_eq!(table, Some((0x40, 11 * 56, 11)));

    let sections = layout.section_headers().expect("a section header table");
    assert_eq!((sections.count, sections.entry_size), (26, 64));

    // No two segments contend for a page, and a vaddr resolves to its file
    // offset — the two derived answers `spawn` refuses a binary over.
    assert_eq!(layout.overlapping_load_pages(4096), None);
    let file_offset = |vaddr| layout.file_offset_of(layout.extent().range(vaddr, 1).unwrap());
    assert_eq!(file_offset(0xc0620), Some(0xbe620));
    assert_eq!(file_offset(0xc1800), Some(0xbe800));
    assert_eq!(file_offset(0x2a8), Some(0x2a8));
}

/// The build-id `object` reads out of `file`'s program headers.
fn objects_build_id(file: &[u8]) -> Option<Vec<u8>> {
    use object::read::elf::{FileHeader, ProgramHeader};
    let header = object::elf::FileHeader64::<object::LittleEndian>::parse(file).expect("object reads the header");
    let endian = header.endian().expect("object reads the endianness");
    for phdr in header.program_headers(endian, file).expect("object reads the program headers") {
        let Some(mut notes) = phdr.notes(endian, file).expect("object reads a note segment") else { continue };
        while let Some(note) = notes.next().expect("object reads a note") {
            if note.name() == b"GNU" && note.n_type(endian) == object::elf::NT_GNU_BUILD_ID {
                return Some(note.desc().to_vec());
            }
        }
    }
    None
}

/// Ours, read as the kernel reads it: each `PT_NOTE` at its own offset.
fn our_build_id(file: &[u8]) -> Option<Vec<u8>> {
    toyos_elf::note::segments(file).find_map(|s| {
        let notes = file.get(s.offset as usize..(s.offset + s.filesz) as usize)?;
        toyos_elf::note::build_id(notes, s.align).map(<[u8]>::to_vec)
    })
}

#[test]
fn an_lld_binarys_build_id_is_what_object_and_readelf_read() {
    Layout::parse(LLD_HEADERS, Machine::X86_64).expect("rust-lld's own output");
    let segments: Vec<_> = toyos_elf::note::segments(LLD_HEADERS).collect();
    // `readelf -l`: `NOTE 0x0002a8 0x00000000000002a8 0x00000000000002a8 0x000024 0x000024 R 0x4`.
    assert_eq!(segments, [toyos_elf::note::NoteSegment { offset: 0x2a8, filesz: 0x24, align: 4 }]);
    let ours = our_build_id(LLD_HEADERS);
    assert_eq!(ours, objects_build_id(LLD_HEADERS));
    // `readelf -n`: `Build ID: 97082d0f339a81a4dadc05e2f47960b079ab178c`.
    let readelf = [
        0x97, 0x08, 0x2d, 0x0f, 0x33, 0x9a, 0x81, 0xa4, 0xda, 0xdc, 0x05, 0xe2, 0xf4, 0x79, 0x60, 0xb0, 0x79,
        0xab, 0x17, 0x8c,
    ];
    assert_eq!(ours.as_deref(), Some(&readelf[..]));
}
