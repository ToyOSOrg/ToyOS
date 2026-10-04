---
status: open
kind: defect
opened: 2026-10-04
---

# A C program clang links for ToyOS carries no build-id

A killed program's frame is recorded with its file's build-id, and
`/system/bin/symbolize` names it only from the file carrying the same one
(`toyos-symbols/src/frame.rs`). The Rust target links with `--build-id`; the
clang ToyOS driver (`clang/lib/Driver/ToolChains/ToyOS.cpp` in the LLVM fork)
passes no `--build-id` to `ld.lld`, so a C program has no `PT_NOTE` and its
frames are recorded `id=-`. The namer then names one from whatever file of that
name it finds, with nothing to tell another build of it apart. Fuchsia's
driver passes the flag (`Fuchsia.cpp`).

**Exit**: a C program the ToyOS clang links carries a GNU build-id note, and a
C case killed by a fault is recorded with it.
