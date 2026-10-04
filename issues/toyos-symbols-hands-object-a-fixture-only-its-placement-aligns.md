---
status: open
kind: defect
opened: 2026-10-04
---

# toyos-symbols hands `object` a fixture only its placement aligns

`toyos-symbols/tests/name.rs`'s `every_function_names_what_object_reads`
parses `BINARY`, an `include_bytes!` of `fixtures/input-test.bin`, with
`object::File::parse`. `include_bytes!` guarantees alignment 1. `object`
0.38, built as the dev-dependency is (no `unaligned` feature), reads the ELF
header in place and refuses a misaligned one: `Bytes::read_at` fails and
`FileHeader::parse` answers "Invalid ELF header size or alignment". The test
passes only because the linker happens to place the bytes on an 8-byte
boundary; a reordering of the test binary's statics can turn it red with no
change to the code it tests. `toyos-elf/tests/real.rs` holds its fixture in a
`#[repr(align(8))]` static for this reason.

**Exit**: the bytes `name.rs` hands `object` are 8-aligned by their type.
