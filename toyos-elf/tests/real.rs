//! The positive control: the parser agrees with a real artifact.
//!
//! Every other test in this crate hands the parser bytes no linker would emit,
//! and a parser that refused everything would pass all of them. The fixture is
//! the first 4 KiB of `/system/bin/shell` as `rust-lld` linked it — which is all
//! [`Layout::parse`] ever reads — so this is the shape the loader actually
//! meets at every boot.
//!
//! Refresh it with `dd if=userland/target/x86_64-unknown-toyos/toyos/shell
//! of=toyos-elf/tests/fixtures/shell-headers.bin bs=1 count=4096`, and expect
//! every number below to move: each is `readelf -lSW` of the whole binary.

use toyos_elf::{Layout, Machine};

const HEADERS: &[u8] = include_bytes!("fixtures/shell-headers.bin");

#[test]
fn a_linked_binary_parses_to_what_readelf_says() {
    let layout = Layout::parse(HEADERS, Machine::X86_64).expect("rust-lld's own output");

    assert_eq!(layout.entry().get(), 0x3772c);
    assert_eq!(layout.extent().min(), 0);
    assert_eq!(layout.extent().max(), 0x97c64);

    // rodata carrying .rela.dyn and .dynsym (R--), text (R-X), the RELRO data
    // carrying .dynamic (RW-), and .data with .bss (RW-).
    let segs = layout.segments();
    assert_eq!(segs.len(), 4);
    assert!(!segs[0].writable() && !segs[0].flags().executable());
    assert!(segs[1].flags().executable() && !segs[1].writable());
    assert!(segs[2].writable() && !segs[2].flags().executable());
    assert!(segs[3].writable() && !segs[3].flags().executable());
    assert_eq!(layout.writable_window(), Some((0x8f860, 0x97c64)));

    let dynamic = layout.dynamic().map(|d| (d.file_offset(), d.image().start().get(), d.image().len()));
    assert_eq!(dynamic, Some((0x90ed8, 0x92ed8, 0x100)));
    let eh = layout.eh_frame_hdr().map(|r| (r.start().get(), r.len()));
    assert_eq!(eh, Some((0x1635c, 0x1e84)));
    assert_eq!(layout.tls().unwrap().memsz(), 0x68);
    assert_eq!(layout.tls().unwrap().align(), 0x8);

    // `e_phoff` 0x40, ten headers of 56 bytes, inside the first segment's file
    // bytes at offset 0.
    let table = layout.program_headers().map(|t| (t.image().start().get(), t.image().len(), t.count()));
    assert_eq!(table, Some((0x40, 10 * 56, 10)));

    let sections = layout.section_headers().expect("a section header table");
    assert_eq!((sections.count, sections.entry_size), (25, 64));

    // Every `DT_*` vaddr in this file resolves, and no two segments contend for
    // a page — the two derived answers `spawn` refuses a binary over. The RW
    // segments sit 0x2000 above their file bytes, and `.rela.dyn` at none.
    assert_eq!(layout.overlapping_load_pages(4096), None);
    assert_eq!(layout.vaddr_to_file_offset(0x92ed8), Some(0x90ed8));
    assert_eq!(layout.vaddr_to_file_offset(0x940a8), Some(0x910a8));
    assert_eq!(layout.vaddr_to_file_offset(0x2b8), Some(0x2b8));
}
