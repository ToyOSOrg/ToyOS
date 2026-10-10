//! The relocator against lld, and every refusal.
//!
//! The oracle is lld itself. `toyos/tests/relocate/fixture.rs` is linked three
//! times per architecture with the sysroot's `rust-lld`, from its object
//! `rustc --edition 2021 --crate-type lib --emit obj -C panic=abort --target
//! <arch>-unknown-toyos fixture.rs` (`RUSTC_BOOTSTRAP=1`, and `--cfg tls` for
//! the third):
//!
//! - `<arch>.elf`: `-flavor gnu -pie --no-dynamic-linker -e _start`;
//! - `<arch>-applied.elf`: the same with `--image-base=0x10000000
//!   --apply-dynamic-relocs`, so lld writes every value itself at the same
//!   file offsets;
//! - `<arch>-tls.elf`: the first line's, of the `--cfg tls` object.
//!
//! The relocator run on the first at bias [`BASE`] writes, at each relocated
//! word, what lld wrote there in the second.

#![allow(
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::arithmetic_side_effects
)]

use super::*;

/// The image base the `-applied` fixtures were linked at.
const BASE: u64 = 0x1000_0000;

const X86_64: &[u8] = include_bytes!("../../tests/relocate/x86_64.elf");
const X86_64_APPLIED: &[u8] = include_bytes!("../../tests/relocate/x86_64-applied.elf");
const X86_64_TLS: &[u8] = include_bytes!("../../tests/relocate/x86_64-tls.elf");
const AARCH64: &[u8] = include_bytes!("../../tests/relocate/aarch64.elf");
const AARCH64_APPLIED: &[u8] = include_bytes!("../../tests/relocate/aarch64-applied.elf");
const AARCH64_TLS: &[u8] = include_bytes!("../../tests/relocate/aarch64-tls.elf");

/// The image `file` loads as, from vaddr 0, which every fixture starts at.
fn memory(file: &[u8]) -> Vec<u8> {
    let image = image_of(file);
    let mut memory = Vec::new();
    for p in image.loads() {
        let end = (p.vaddr + p.memsz) as usize;
        if memory.len() < end {
            memory.resize(end, 0);
        }
        let (at, len) = (p.vaddr as usize, p.filesz as usize);
        memory[at..at + len].copy_from_slice(&file[p.offset as usize..p.offset as usize + len]);
    }
    memory
}

fn image_of(file: &[u8]) -> Image<'_> {
    let header = Header::parse(&file[..HEADER_SIZE]).expect("lld's header");
    Image::new(header, &file[header.phoff..header.phoff + header.phlen]).expect("lld's program headers")
}

/// Every write the relocator makes of `file` at `bias`, as the wrapper
/// reaches the tables: through the loaded image, by vaddr.
fn writes(file: &[u8], bias: u64) -> Result<Vec<(u64, u64)>, Refusal> {
    let image = image_of(file);
    let memory = memory(file);
    let slice = |span: Span| &memory[span.vaddr as usize..span.vaddr as usize + span.len];
    let dynamic = image.dynamic()?.expect("a PT_DYNAMIC");
    let rela = image.rela(slice(dynamic))?.expect("a DT_RELA");
    let mut out = Vec::new();
    image.apply(slice(rela), bias, |vaddr, value| out.push((vaddr, value)))?;
    Ok(out)
}

/// The file offset `vaddr` is read from.
fn file_offset(file: &[u8], vaddr: u64) -> usize {
    let p = image_of(file).loads().find(|p| p.holds(vaddr, 8, p.filesz)).expect("a file-backed vaddr");
    (p.offset + (vaddr - p.vaddr)) as usize
}

fn word(file: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(file[at..at + 8].try_into().unwrap())
}

fn agrees_with_lld(plain: &[u8], applied: &[u8], count: usize) {
    let writes = writes(plain, BASE).expect("lld's own output applies");
    assert_eq!(writes.len(), count, "one write for each of the fixture's RELATIVE entries");
    for (vaddr, value) in writes {
        let at = file_offset(plain, vaddr);
        assert_eq!(value, word(applied, at), "the word at {vaddr:#x} is not what lld wrote there");
        assert_ne!(value, word(plain, at), "lld wrote {vaddr:#x} into the file too, so it tests nothing");
    }
}

#[test]
fn x86_64_writes_what_lld_writes() {
    // `readelf -r x86_64.elf`: nine `R_X86_64_RELATIVE`.
    agrees_with_lld(X86_64, X86_64_APPLIED, 9);
}

#[test]
fn aarch64_writes_what_lld_writes() {
    // `readelf -r aarch64.elf`: nine `R_AARCH64_RELATIVE`.
    agrees_with_lld(AARCH64, AARCH64_APPLIED, 9);
}

#[test]
fn a_pointer_in_the_tls_template_is_refused() {
    // `readelf -lr x86_64-tls.elf`: `TLS 0x000478 0x2478 … 0x10` and a
    // `R_X86_64_RELATIVE` at `0x2478`; `aarch64-tls.elf` the same at `0x204e0`.
    assert_eq!(writes(X86_64_TLS, BASE), Err(Refusal::InTlsTemplate));
    assert_eq!(writes(AARCH64_TLS, BASE), Err(Refusal::InTlsTemplate));
}

/// `tags`, `DT_NULL`-terminated, as a dynamic section's bytes.
fn dynamic(tags: &[(u64, u64)]) -> Vec<u8> {
    let mut out = Vec::new();
    for &(tag, value) in tags.iter().chain(&[(DT_NULL, 0)]) {
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

/// One `Elf64_Rela`.
fn rela(offset: u64, kind: u64, addend: u64) -> Vec<u8> {
    [offset, kind, addend].iter().flat_map(|w| w.to_le_bytes()).collect()
}

/// The x86-64 fixture's own table, with `tags` beside it.
fn x86_tables(tags: &[(u64, u64)]) -> Result<Option<Span>, Refusal> {
    // `readelf -d x86_64.elf`: `RELA 0x210`, `RELASZ 216`.
    let own = [(DT_RELA, 0x210), (DT_RELASZ, 216), (DT_RELAENT, 24)];
    image_of(X86_64).rela(&dynamic(&[&own[..], tags].concat()))
}

#[test]
fn every_dynamic_form_it_does_not_apply_is_refused_by_name() {
    assert_eq!(x86_tables(&[]), Ok(Some(Span { vaddr: 0x210, len: 216 })));
    assert_eq!(x86_tables(&[(DT_NEEDED, 1)]), Err(Refusal::Needed));
    assert_eq!(x86_tables(&[(23, 0x210), (DT_PLTRELSZ, 24)]), Err(Refusal::JmpRel));
    // A `DT_JMPREL` with no entries binds nothing.
    assert!(x86_tables(&[(23, 0x210), (DT_PLTRELSZ, 0)]).is_ok());
    assert_eq!(x86_tables(&[(DT_REL, 0x210)]), Err(Refusal::Rel));
    assert_eq!(x86_tables(&[(DT_RELR, 0x210)]), Err(Refusal::Relr));
    assert_eq!(x86_tables(&[(DT_TEXTREL, 0)]), Err(Refusal::TextRel));
    assert_eq!(x86_tables(&[(DT_FLAGS, DF_TEXTREL)]), Err(Refusal::TextRel));
    // `DF_BIND_NOW` alone is no write into text.
    assert!(x86_tables(&[(DT_FLAGS, 8)]).is_ok());
}

#[test]
fn a_rela_table_of_the_wrong_shape_or_place_is_refused() {
    let image = image_of(X86_64);
    assert_eq!(image.rela(&dynamic(&[])), Ok(None));
    assert_eq!(image.rela(&dynamic(&[(DT_RELA, 0x210)])), Err(Refusal::RelaShape));
    assert_eq!(image.rela(&dynamic(&[(DT_RELASZ, 24)])), Err(Refusal::RelaShape));
    assert_eq!(image.rela(&dynamic(&[(DT_RELA, 0x210), (DT_RELASZ, 25)])), Err(Refusal::RelaShape));
    assert_eq!(image.rela(&dynamic(&[(DT_RELA, 0x210), (DT_RELASZ, 24), (DT_RELAENT, 16)])), Err(Refusal::RelaShape));
    // In the writable segment, `[0x2428, 0x2598)` in its file bytes.
    assert_eq!(image.rela(&dynamic(&[(DT_RELA, 0x2440), (DT_RELASZ, 24)])), Err(Refusal::RelaPlacement));
    // Past the read-only segment's file bytes, `[0, 0x3cc)`, into the gap before text.
    assert_eq!(image.rela(&dynamic(&[(DT_RELA, 0x3c0), (DT_RELASZ, 24)])), Err(Refusal::RelaPlacement));
    assert_eq!(image.rela(&dynamic(&[(DT_RELA, u64::MAX - 8), (DT_RELASZ, 24)])), Err(Refusal::RelaPlacement));
}

fn applied(file: &[u8], entries: &[Vec<u8>]) -> Result<Vec<(u64, u64)>, Refusal> {
    let mut out = Vec::new();
    image_of(file).apply(&entries.concat(), BASE, |vaddr, value| out.push((vaddr, value)))?;
    Ok(out)
}

#[test]
fn a_write_is_bias_plus_addend_inside_a_writable_segment() {
    // The writable segment is `[0x2428, 0x3000)` in memory, `.bss` included.
    assert_eq!(applied(X86_64, &[rela(0x2428, 8, 0x13d0)]), Ok(vec![(0x2428, BASE + 0x13d0)]));
    assert_eq!(applied(X86_64, &[rela(0x2ff8, 8, 0)]), Ok(vec![(0x2ff8, BASE)]));
    // An addend is two's complement, as `r_addend` is signed.
    assert_eq!(applied(X86_64, &[rela(0x2428, 8, (-16i64) as u64)]), Ok(vec![(0x2428, BASE - 16)]));
}

#[test]
fn a_write_outside_a_writable_segment_is_refused() {
    // Into text, into the read-only headers, and across the writable segment's end.
    for offset in [0x13d0, 0x40, 0x2ffc, 0x2424, u64::MAX - 4] {
        assert_eq!(applied(X86_64, &[rela(offset, 8, 0)]), Err(Refusal::OutsideWritable), "{offset:#x}");
    }
}

#[test]
fn a_write_meeting_the_tls_template_is_refused() {
    // `x86_64-tls.elf`'s template is `[0x2478, 0x2488)`; an 8-byte write
    // starting 4 bytes before its end meets it.
    for offset in [0x2478, 0x2480, 0x2484] {
        assert_eq!(applied(X86_64_TLS, &[rela(offset, 8, 0)]), Err(Refusal::InTlsTemplate), "{offset:#x}");
    }
    assert!(applied(X86_64_TLS, &[rela(0x2488, 8, 0)]).is_ok());
}

#[test]
fn every_type_but_the_machines_relative_is_refused() {
    // `R_X86_64_JUMP_SLOT`, `GLOB_DAT`, `TPOFF64`, `NONE`, and AArch64's `RELATIVE`.
    for kind in [7, 6, 18, 0, 1027] {
        assert_eq!(applied(X86_64, &[rela(0x2428, kind, 0)]), Err(Refusal::Kind(kind as u32)));
    }
    // `R_AARCH64_JUMP_SLOT` and x86-64's `RELATIVE` in an AArch64 image.
    for kind in [1026, 8] {
        assert_eq!(applied(AARCH64, &[rela(0x20460, kind, 0)]), Err(Refusal::Kind(kind as u32)));
    }
    // The first refusal stops it: nothing after it is written.
    let mut out = Vec::new();
    let refused = image_of(X86_64).apply(&[rela(0x2428, 7, 0), rela(0x2430, 8, 0)].concat(), BASE, |v, w| out.push((v, w)));
    assert_eq!((refused, out), (Err(Refusal::Kind(7)), vec![]));
}

/// The x86-64 fixture's header with `edit` applied to its bytes.
fn header_of(edit: impl FnOnce(&mut [u8])) -> Result<Header, Refusal> {
    let mut head = X86_64[..HEADER_SIZE].to_vec();
    edit(&mut head);
    Header::parse(&head)
}

#[test]
fn a_header_this_does_not_know_is_refused() {
    assert_eq!(header_of(|_| {}), Ok(Header { phoff: 64, phlen: 7 * 56, relative: R_X86_64_RELATIVE }));
    assert_eq!(header_of(|h| h[0] = 0), Err(Refusal::Header));
    // ELFCLASS32, and big-endian.
    assert_eq!(header_of(|h| h[4] = 1), Err(Refusal::Header));
    assert_eq!(header_of(|h| h[5] = 2), Err(Refusal::Header));
    // EM_386.
    assert_eq!(header_of(|h| h[18] = 3), Err(Refusal::Header));
    // 32-byte program headers.
    assert_eq!(header_of(|h| h[54] = 32), Err(Refusal::Header));
    assert_eq!(Header::parse(&X86_64[..32]), Err(Refusal::Header));
    let aarch64 = Header::parse(&AARCH64[..HEADER_SIZE]).map(|h| h.relative);
    assert_eq!(aarch64, Ok(R_AARCH64_RELATIVE));
}

/// The x86-64 fixture's program headers with `edit` applied to each.
fn image_with(edit: impl Fn(usize, &mut [u8])) -> Result<u64, Refusal> {
    let header = Header::parse(&X86_64[..HEADER_SIZE]).unwrap();
    let mut phdrs = X86_64[header.phoff..header.phoff + header.phlen].to_vec();
    for (i, entry) in phdrs.chunks_exact_mut(PHDR_SIZE).enumerate() {
        edit(i, entry);
    }
    Image::new(header, &phdrs).map(|image| image.header_vaddr)
}

/// `readelf -l x86_64.elf`: `PHDR`, then the read-only `LOAD` at offset 0
/// whose file bytes are `0x3cc`.
const HEADER_LOAD: usize = 1;

#[test]
fn a_header_no_read_only_segment_holds_is_refused() {
    assert_eq!(image_with(|_, _| {}), Ok(0));
    // Its segment starts one byte into the file.
    assert_eq!(image_with(|i, p| if i == HEADER_LOAD { p[8] = 1 }), Err(Refusal::HeaderSegment));
    // Its file bytes end inside the program header table.
    assert_eq!(image_with(|i, p| if i == HEADER_LOAD { p[32..40].copy_from_slice(&0x100u64.to_le_bytes()) }), Err(Refusal::HeaderSegment));
    // It is writable.
    assert_eq!(image_with(|i, p| if i == HEADER_LOAD { p[4] |= PF_W as u8 }), Err(Refusal::HeaderSegment));
}

#[test]
fn a_dynamic_section_in_no_file_bytes_is_refused() {
    // `DYNAMIC 0x0004c8 0x24c8 … 0xd0` is the fixture's fifth program header;
    // moved past the writable segment's file bytes, `[0x2428, 0x2598)`, into its `.bss`.
    let image_dynamic = |vaddr: u64| {
        let header = Header::parse(&X86_64[..HEADER_SIZE]).unwrap();
        let mut phdrs = X86_64[header.phoff..header.phoff + header.phlen].to_vec();
        let entry = &mut phdrs[4 * PHDR_SIZE..5 * PHDR_SIZE];
        assert_eq!(u32::from_le_bytes(entry[..4].try_into().unwrap()), PT_DYNAMIC);
        entry[16..24].copy_from_slice(&vaddr.to_le_bytes());
        Image::new(header, &phdrs).unwrap().dynamic()
    };
    assert_eq!(image_dynamic(0x24c8), Ok(Some(Span { vaddr: 0x24c8, len: 0xd0 })));
    assert_eq!(image_dynamic(0x2600), Err(Refusal::Dynamic));
}
