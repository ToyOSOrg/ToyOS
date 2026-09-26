---
status: open
kind: defect
opened: 2026-09-26
---

# toyos-ld moves a pointer past a merged string onto another string

`merge_string_sections` (`toyos-ld/src/collect.rs`) deduplicates the pieces of
every `SHF_MERGE | SHF_STRINGS` section with `entsize = 1` and then remaps
each relocation into one through `offset_remap`, a map keyed on the
**input offset of each piece's first byte**. A relocation whose target is not
a piece's first byte is handled two wrong ways, and neither is refused:

- **One past a piece's end** is the next piece's first byte, so it is remapped
  onto wherever *that* piece landed. When the next piece was a duplicate of an
  earlier string, that is somewhere else entirely.
- **Inside a piece**, the offset is in no map and the addend is left as it
  was, relative to a symbol that has itself moved.

Both are silent: the output links and runs wrong.

**Seen**: netd built on `wt/toyos-netstack3`, `dhcp::frame` writing an
EtherType with `f.extend_from_slice(&[0x08, 0x00])`. rustc emits the two-byte
array as a C string (`"\x08\0"`, in `.rodata.str1.1`) and folds the slice's end
into a relocation one past it. In the linked binary the start is `0x1ba14` and
the end `0x15c18` (`llvm-objdump -d`, `netd+0x3c2d7d`..`0x3c2d95`), and the
first DISCOVER killed netd with core's own check:

    panicked at library/core/src/ptr/non_null.rs:898:32:
    unsafe precondition(s) violated: ptr::offset_from_unsigned requires `self >= origin`
      11: <Vec<u8> as SpecExtend<&u8, slice::Iter<u8>>>::spec_extend
      12: netd::dhcp::frame

The same code passes on the host (`cargo test --target aarch64-apple-darwin`,
linked by the system linker).

**What would close it**: an input section is merged only where every relocation into it lands on a piece's first byte, and one any relocation reaches past a first byte keeps its bytes and its place whole — a pointer one past a piece's end is indistinguishable from the next piece's start once the relocation is against the section and an offset, so the only honest layout for such a section is its own. The negative control is an object whose string section holds two pieces, the second a duplicate of a string in another object, referenced one past the first's end: red before, green after.
