---
status: open
kind: defect
opened: 2026-09-26
---

# `dlopen` loads a library whose `DT_RELR` table it never reads

`toyos_elf::dynamic::Dynamic::parse` decodes the tags the kernel's `dlopen`
acts on and drops every other one (`_ => {}`), so a library linked with
`-z pack-relative-relocs` loads with its relative relocations — the ones
`DT_RELR`/`DT_RELRSZ` name, and that `DT_RELA` then no longer holds — never
applied. `DT_REL` and `DT_TEXTREL` are dropped the same way. The process runs
with every pointer in the library's data still the link-time one and dies
somewhere after, or does not die.

Nothing in the tree links that way today: rustc passes `rust-lld` no
`--pack-dyn-relocs`, and LLD's default is none. An executable carrying any of
the three is refused at its own start (`toyos::relocate`); a library is input
from outside the kernel, and `dlopen` has no such refusal.

Exit: a dynamic section naming a relocation form `dlopen` does not apply is
refused at `dlopen`, and a crafted library carrying `DT_RELR` is the test.
