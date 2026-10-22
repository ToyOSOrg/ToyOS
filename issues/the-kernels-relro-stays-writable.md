---
status: open
kind: defect
opened: 2026-10-21
---

# The kernel's RELRO stays writable

The kernel maps its own image page by page from its `PT_LOAD` flags
(`kernel/src/mm/image.rs`), so its `PT_GNU_RELRO` — `.data.rel.ro`, `.got` and
`.dynamic`, about 70 KiB on x86-64 — is read-write and execute-never for the
machine's life. The loader applies the image's relocations before the kernel
runs, so nothing needs to write it afterwards, and a stray kernel write can
rewrite a vtable or a function pointer there. Linux at the parity tag of
`issues/the-kernel-is-at-least-as-secure-as-linux-on-every-machine-toyos-supports.md`
maps the same data read-only. `toyos-elf` parses no `PT_GNU_RELRO`, which
`issues/an-executables-relro-stays-writable.md` needs too.

Exit: the kernel's `PT_GNU_RELRO` pages are read-only in its tables from
`mm::init` on, and a boot check or test write shows it.
