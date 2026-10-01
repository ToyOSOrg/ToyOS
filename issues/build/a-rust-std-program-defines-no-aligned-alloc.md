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

Std's `free` and `realloc` release every block at alignment 16, the one its
`malloc` allocates at. The allocator under them, dlmalloc
(`rust/library/std/src/sys/alloc/toyos.rs`), checks a released block's size and
ignores its alignment, so a block allocated at a larger one and released at 16
is a layout mismatch nothing reports.

**Exit**: std's C allocator defines `aligned_alloc`, whose block's address is a
multiple of the requested alignment, and `free` and `realloc` release each
block at the layout it was allocated with, the block's header carrying its
alignment as libc's own allocator's does. A guest case in a Rust std program
allocates through `aligned_alloc` at alignments 64 and 4096, asserts each
address a multiple of its alignment, and frees each.
