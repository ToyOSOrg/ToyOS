---
status: open
kind: defect
opened: 2026-09-29
---

# A Rust std executable runs no `.init_array`, so a C constructor linked into it is skipped

`rust/library/std/src/sys/pal/toyos/mod.rs:93` starts a std binary at
`start_rust`, which calls `main` directly and never walks `.init_array`
(the comment there: "exes don't run .init_array"). `userland/libc/src/lib.rs`'s
`start_c` does walk it, but only `#[cfg(not(feature = "std-runtime"))]`: a
Rust program with std never links that module.

A C constructor linked into such a binary — doom's C half built by `cc`, or a
crate using the `ctor` crate — is silently skipped: it is placed in
`.init_array` by the compiler and nothing ever runs the array.

**Exit**: a std executable with a linked C (or `ctor`) constructor runs it.
