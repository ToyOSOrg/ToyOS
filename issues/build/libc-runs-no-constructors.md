---
status: open
kind: defect
opened: 2026-09-27
---

# libc runs no constructors and no destructors

A C program's `__attribute__((constructor))` and `((destructor))` functions go
into `.init_array` and `.fini_array`, and libc's entry
(`userland/libc/src/lib.rs`'s `start_c`) calls `main` without running either, so
they never run and nothing says so. lld defines `__init_array_start` and
`__init_array_end` for a start routine to walk.

toyos-cc refused the attribute by name, so no program here had one. clang
compiles it, and TinyCC's `108_constructor` prints `main` where it should print
`constructor`, `main`, `destructor`; it is declined in `tests/toyos.rs`'s
`NOT_RUN` with this entry named.

**Exit**: libc's start runs `.init_array` before `main` and `exit` runs
`.fini_array` after the `atexit` handlers, and `108_constructor` runs.
