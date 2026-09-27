---
status: open
kind: defect
opened: 2026-09-26
---

# The program loader runs a binary whose `DT_RELR` table it never reads

`toyos_elf::dynamic::Dynamic::parse` decodes the tags the loader acts on and
drops every other one (`_ => {}`), so a program or library linked with
`-z pack-relative-relocs` loads with its relative relocations — the ones
`DT_RELR`/`DT_RELRSZ` name, and that `DT_RELA` then no longer holds — never
applied. `DT_REL` and `DT_TEXTREL` are dropped the same way. The process runs
with every pointer in its data still the link-time one and dies somewhere
after, or does not die.

Nothing in the tree links that way today: rustc passes `rust-lld` no
`--pack-dyn-relocs`, and LLD's default is none. The bootloader refuses the
kernel's equivalent (`SHT_RELR`) by name; the program loader has no such
refusal, and a binary is input from outside the kernel.

Exit: a dynamic section naming a relocation form the loader does not apply is
refused at spawn and at `dlopen`, and a crafted binary carrying `DT_RELR` is the
test.
