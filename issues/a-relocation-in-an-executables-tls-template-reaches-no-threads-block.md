---
status: open
kind: defect
opened: 2026-10-10
---

# A relocation in an executable's TLS template reaches no thread's block

The kernel copies an executable's `PT_TLS` template out of the file at spawn
(`kernel/src/loader/mod.rs`, `elf_alloc`) and builds every thread's TLS block
from that copy, while it applies the executable's `RELATIVE` relocations to the
image's pages as they fault in (`kernel/src/elf/index.rs`). A `RELATIVE` whose
`r_offset` lies in `[PT_TLS vaddr, + filesz)` relocates the image's `.tdata`
and never the copies: every thread reads the link-time pointer.

rust-lld writes one for a `#[thread_local]` static holding a `&'static str`:
`readelf -lr` of such a static PIE for `x86_64-unknown-toyos` reads
`TLS 0x000478 0x2478 … 0x10` and an `R_X86_64_RELATIVE` at `0x2478`. None of
the shipped programs carries one, nor clang or LLD.

Owner: stage 1 of `issues/the-kernel-still-parses-what-userland-writes.md`.

Exit: an executable with a relocation in its TLS template is refused, and a
host test on rust-lld's own output of one shows it.
