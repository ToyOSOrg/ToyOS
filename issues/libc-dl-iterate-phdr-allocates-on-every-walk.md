---
status: open
kind: defect
opened: 2026-09-29
---

# Nothing holds that libc's `dl_iterate_phdr` walks without allocating

libunwind calls `dl_iterate_phdr` to find a frame's unwind tables, so a walk
that allocates lets a C++ `throw std::bad_alloc` with the heap exhausted die
in libc's allocator instead of reaching its handler. `sdk/libc/src/link.rs`
walks the process's own module list (`toyos::tls`), whose names are kept
NUL-terminated, and allocates nothing, but no case shows it.

**Exit**: a guest C case walks every module with `malloc` exhausted and
visits each.
