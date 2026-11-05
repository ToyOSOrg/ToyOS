---
status: open
kind: defect
opened: 2026-10-10
---

# An executable's RELRO stays writable for its whole life

`PT_GNU_RELRO` names what a program's relocations write once and nothing
writes after: `.data.rel.ro`, `.dynamic`, `.got`, every vtable and every table
of function pointers. A parent lays it out as part of the writable range that
holds it (`toyos::elf::program`), which the kernel maps as asked; nothing reads
an executable's `PT_GNU_RELRO`, and nothing takes the write away once
`toyos::relocate` has run. A stray or hostile write in a running program can
rewrite a vtable. A library's is read-only once the SDK's loader has relocated
it (`sdk/toyos/src/dl/mod.rs`, `protect`); an executable's is not, and the same
`SYS_VM_PROTECT` is what its start would call.

Owner: the userland loader of
`issues/the-kernel-still-parses-what-userland-writes.md`.

Exit: once a program has relocated itself its `PT_GNU_RELRO` range is
read-only, through a protection primitive the program calls or a mapping
granular enough to hold it apart, and a guest test's write to it is refused.
