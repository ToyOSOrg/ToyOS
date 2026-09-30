---
status: open
kind: defect
opened: 2026-09-30
---

# libc's mmap ignores the file it is asked to map

`mmap` (`userland/libc/src/posix_io.rs`) never reads its `fd` or `offset`, and
maps anonymous memory: a file mapping succeeds and reads zeros. LLVM maps a
file it reads when it is at least four pages and needs no terminator past the
mapping's end (`llvm/lib/Support/MemoryBuffer.cpp`'s `shouldUseMmap`, with
libc's `sysconf(_SC_PAGESIZE)` answering 4096), so an LLVM on ToyOS reads such
a file as zeros and reports no error.

**Exit**: `mmap` with a file descriptor maps that file's bytes from `offset`, or
fails with `errno` set, and a guest case maps a file of at least 16 KiB and
reads its bytes back.
