---
status: open
kind: defect
opened: 2026-09-30
---

# A Rust std program defines no aligned_alloc

In a Rust std program the C allocator is std's:
`rust/library/std/src/sys/pal/toyos/mod.rs` defines `malloc`, `calloc`,
`realloc` and `free`, and libc's allocator is not built beside std
(`userland/libc/src/memory.rs`, `feature = "std-runtime"`). Nothing defines
C11's `aligned_alloc`, which libc++abi calls for an over-aligned `operator new`
and in its fallback allocator: the link
`issues/build/a-rust-std-binary-cannot-link-the-cxx-runtime.md` describes
leaves it undefined, referenced from `libc++.a`'s `stdlib_new_delete.cpp` and
`fallback_malloc.cpp`.

**Exit**: a Rust std program's C code allocates through `aligned_alloc` from
std's allocator and frees through `free`, and a guest case does it at an
alignment above 16.
