---
status: open
kind: defect
opened: 2026-10-10
---

# An executable's RELRO stays writable for its whole life

`PT_GNU_RELRO` names what a program's relocations write once and nothing
writes after: `.data.rel.ro`, `.dynamic`, `.got`, every vtable and every table
of function pointers. The kernel maps it as the writable `PT_LOAD` that holds
it, reads no `PT_GNU_RELRO` (`rg -i relro kernel/src toyos-elf/src` matches
nothing), and gives a program no way to take the write away once
`toyos::relocate` has run. A stray or hostile write in a running program can
rewrite a vtable.

Owner: the userland loader of
`issues/the-kernel-still-parses-what-userland-writes.md`.

Exit: once a program has relocated itself its `PT_GNU_RELRO` range is
read-only, through a protection primitive the program calls or a mapping
granular enough to hold it apart, and a guest test's write to it is refused.
