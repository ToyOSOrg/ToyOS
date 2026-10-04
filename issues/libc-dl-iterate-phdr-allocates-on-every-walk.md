---
status: open
kind: defect
opened: 2026-09-29
---

# libc's `dl_iterate_phdr` allocates on every walk

`userland/libc/src/link.rs` sizes and reads a whole `SYS_QUERY_MODULES`
answer into a heap `Vec`, and builds each module's NUL-terminated name in a
second one: every walk allocates twice and makes at least two syscalls.
libunwind calls `dl_iterate_phdr` to find a frame's unwind tables, so a C++
`throw std::bad_alloc` with the heap exhausted dies in libc's allocator
instead of reaching its handler. The answer's size grows with every library
and path, so no fixed buffer holds it: an allocation-free walk needs a query whose
answer a bounded buffer holds, which is an ABI change.

**Exit**: `dl_iterate_phdr` makes no heap allocation, and a guest C case walks
every module with `malloc` exhausted and visits each.
