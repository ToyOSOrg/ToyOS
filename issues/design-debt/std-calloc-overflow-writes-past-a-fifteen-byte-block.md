---
status: open
kind: defect
opened: 2026-09-26
---

# std's C `calloc` answers an overflowing request with a pointer past its block

`library/std/src/sys/pal/toyos/mod.rs`'s `c_allocator` is the `malloc`,
`calloc`, `free` and `realloc` of every userland program: std defines them
for the Rust crates that call C's allocator, and `/system/bin/doom`'s C gets
them too, because `userland/libc` defines none of the four when it is built
beside std (`std-runtime`).

`calloc` sizes its request with `count.saturating_mul(size)`, so an overflowing
product asks `malloc` for `usize::MAX` bytes. `malloc` then computes
`HEADER + size` unchecked; std is built without overflow checks, so that wraps
to 15. It allocates 15 bytes, stores 15 in the header and returns the header's
address plus 16 — one byte past the block. `calloc` sees a non-null pointer and
a non-zero size and zeroes `usize::MAX` bytes from there.

C's contract is a null pointer for a product that overflows; `userland/libc`'s
own `calloc` does that with `checked_mul`. Every `malloc(n)` with `n` within 16
of `usize::MAX` takes the same wrapped path.

Exit: `calloc` returns null on an overflowing product, `malloc` refuses a size
whose header does not fit, and a guest test calls both with a size that
overflows.
