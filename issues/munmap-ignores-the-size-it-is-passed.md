---
status: open
kind: defect
opened: 2026-10-07
---

# `SYS_MUNMAP` ignores the size it is passed

`kernel/src/syscall/vm.rs`'s `sys_munmap(addr: u64, _size: u64)` finds the
mapping whose start is `addr` and frees all of it; the second argument is never
read. `toyos-abi`'s `munmap(addr, size)` documents that the pair must describe
a region `mmap` returned, and nothing holds a caller to it.

Read at `f260e0b98`, nothing run: `userland/libc/src/posix_io.rs`'s `munmap`
forwards a C caller's `len` unchanged, and POSIX lets that caller unmap a
prefix of a mapping. Such a call answers 0 and takes the whole mapping, the
part the caller kept included, so the program's next touch of it faults. A
`size` larger than the mapping is answered 0 as well.

The common mismatch is not that one and is correct: `sys_mmap` rounds the
request up to a 2 MiB span and `MmapRegion.size` holds the rounded figure, so
a caller that unmaps the length it mapped passes a size the kernel never
recorded. `tests/testcases/tinycc/119_random_stuff.c` maps 4096 and unmaps
4096. A refusal that compared the argument to `MmapRegion.size` as passed
would refuse every such call.

## Exit condition

A size is refused unless, rounded as `sys_mmap` rounds its request, it is the
mapping's recorded size, which is how this file reads the ruling in
`issues/process-memory-is-2-mib-pages-and-that-caps-the-process-count.md`'s
stage 4; and a test that maps two 2 MiB spans and unmaps the first reads the
refusal and then reads its mapping back, and one that maps 4096 and unmaps
4096 reads 0. This file is deleted with that.

## Owner

`kernel/src/syscall/vm.rs`, `toyos-abi/src/syscall.rs`, `userland/libc`. Nobody holds it.
