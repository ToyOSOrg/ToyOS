---
status: open
kind: defect
opened: 2026-09-30
---

# libc's mmap ignores the file it is asked to map

`mmap` (`userland/libc/src/posix_io.rs`) never reads its `fd` or `offset`, and
maps anonymous memory, `MAP_SHARED` as `MAP_PRIVATE`: a file mapping succeeds
and reads zeros, and what is written through a shared one never reaches the
file. The kernel maps no file (`kernel/src/syscall/vm.rs`). Read from LLVM at
`849da7d62`, not run:

- `MemoryBuffer` maps a file it reads, private and read-only, when the file is
  at least four pages and needs no terminator past the mapping's end
  (`llvm/lib/Support/MemoryBuffer.cpp`'s `shouldUseMmap`, with libc's
  `sysconf(_SC_PAGESIZE)` answering 4096), so it reads such a file as zeros
  and reports no error.
- `FileOutputBuffer`, through which lld writes its output, maps the output file
  shared and writable (`llvm/lib/Support/FileOutputBuffer.cpp`), so lld writes
  an output of zeros and reports success.

Each falls back, to `read` and to a buffer in memory, when the map fails.

**Exit**: `mmap` of a file, shared or private, at offset 0 or at a later page,
answers `MAP_FAILED` with `errno` `ENODEV`, and a guest C case asserts each and
that an anonymous mapping still maps.
