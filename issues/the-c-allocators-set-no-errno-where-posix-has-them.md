---
status: open
kind: defect
opened: 2026-10-01
---

# The C allocators set no errno where POSIX has them

Read from the code, not run. POSIX.1-2024 has `malloc`, `calloc`, `realloc`
and `aligned_alloc` set `ENOMEM` when they answer null for want of memory, and
`aligned_alloc` set `EINVAL` for an alignment it does not support.

There is one C allocator (`sdk/libc/src/malloc.rs`). libc compiles it into a
C program, where it sets both. In a Rust program it is std's copy
(`sdk/std/sys/pal/mod.rs`), whose `refused` sets `errno` in none of the four.
`errno` is libc's (`sdk/libc/src/errno.rs`), and std names no libc symbol:
setting it from std puts `__errno_location` in every Rust program's link.

`realloc(p, 0)` frees `p` and answers null with `errno` untouched.
POSIX.1-2024 has it answer either null with `errno` `EINVAL`, or a pointer
with `p` freed; its
application usage frees `p` only if `errno` changed, so `EINVAL` beside the
free invites a double free. LLVM's `safe_realloc`
(`llvm/include/llvm/Support/MemAlloc.h`) takes a null answer to size 0 as `p`
freed and allocates afresh, so null with `p` kept leaks it there.

**Exit**: each of the four sets `errno` where POSIX has it in a Rust program,
and a C call there reads `ENOMEM` and `EINVAL` back; `realloc(p, 0)` gives
POSIX's second answer: a pointer, with `p` freed.
