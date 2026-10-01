---
status: open
kind: defect
opened: 2026-10-01
---

# The C allocators set no errno where POSIX has them

Read from the code, not run. POSIX.1-2024 has `malloc`, `calloc`, `realloc`
and `aligned_alloc` set `ENOMEM` when they answer null for want of memory, and
`aligned_alloc` set `EINVAL` for an alignment it does not support.

- In a Rust program, C code allocates through std's C allocator
  (`rust/library/std/src/sys/pal/toyos/mod.rs`, `c_allocator`), which sets
  `errno` in none of the four. `errno` is libc's (`userland/libc/src/errno.rs`),
  and std names no libc symbol: setting it from std puts `__errno_location` in
  every Rust program's link.
- In a C program, libc's own allocator (`userland/libc/src/memory.rs`) sets it
  in `aligned_alloc` alone.

**Exit**: each of the four sets `errno` where POSIX has it, in a C program and
in a Rust one, and a C call in each reads `ENOMEM` and `EINVAL` back.
