//! The positive control: `locate` agrees with an outside judge on a real
//! binary's symbol table.
//!
//! Every other property this crate could assert is a property of hostile or
//! synthetic bytes, and a parser that refused everything would pass those
//! trivially. This is the shape `kernel/src/symbols.rs` actually meets at
//! every boot: `/bin/input-test` as `toyos-ld` linked it, whole, with the
//! judge being GNU `readelf`/`nm` (`/opt/homebrew/opt/binutils/bin`, a distinct
//! implementation of the ELF spec from `toyos-elf`) rather than anything this
//! tree wrote.
//!
//! Refresh it with:
//! ```text
//! cargo run -- --build-only
//! cp userland/target/x86_64-unknown-toyos/toyos/input-test \
//!    toyos-symbols/tests/fixtures/input-test.bin
//! ```
//! and expect every number below to move — `readelf -S`/`readelf --syms`/`nm`
//! against the fresh binary is what re-derives them.

use toyos_elf::sym::SymTab;

const BINARY: &[u8] = include_bytes!("fixtures/input-test.bin");

#[test]
fn locate_finds_the_same_tables_readelf_reports() {
    let (symtab, strtab) = toyos_symbols::locate(BINARY).expect("input-test has a symtab");

    // `readelf -S userland/target/x86_64-unknown-toyos/toyos/input-test`:
    //   [ 6] .symtab  SYMTAB  ...  00016ea8  ...
    //   [ 7] .strtab  STRTAB  ...  0004b65f  ...
    assert_eq!(symtab.len(), 0x16ea8);
    assert_eq!(strtab.len(), 0x4b65f);
}

#[test]
fn a_real_binarys_own_functions_resolve_to_their_own_names() {
    let (symtab, strtab) = toyos_symbols::locate(BINARY).expect("input-test has a symtab");
    let table = SymTab::new(symtab, strtab);

    // `readelf --syms`: `3888: 0000000000005a0c  16 FUNC GLOBAL DEFAULT 1 _start`
    // — the file's own entry point, at `readelf -h`'s "Entry point address".
    assert_eq!(table.resolve(0x5a0c), Some(("_start", 0)));
    assert_eq!(table.resolve(0x5a0c + 8), Some(("_start", 8)));
    // One past `_start`'s last byte (`value + size` = `0x5a0c + 16`) belongs to
    // whatever follows it in the table, never to `_start` itself — the same
    // rule `kernel/src/symbols.rs::SymbolTable::resolve_return` leans on to
    // keep a tail-called function's return address from naming the wrong
    // frame.
    assert_ne!(table.resolve(0x5a0c + 16), Some(("_start", 16)));

    // `readelf --syms`: `3902: 00000000000013a0  55 FUNC GLOBAL DEFAULT 1 main`
    assert_eq!(table.resolve(0x13a0), Some(("main", 0)));
    assert_eq!(table.resolve(0x13a0 + 54), Some(("main", 54)));
}

#[test]
fn a_file_header_locate_cannot_read_answers_none() {
    assert_eq!(toyos_symbols::locate(&BINARY[..32]), None);
    assert_eq!(toyos_symbols::locate(&[]), None);
}
