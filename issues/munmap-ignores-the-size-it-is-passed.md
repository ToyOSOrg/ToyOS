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

## Exit condition

A size that is not the whole mapping's is refused, which
`issues/process-memory-is-2-mib-pages-and-that-caps-the-process-count.md`'s
stage 4 already rules, or `SYS_MUNMAP` takes no size; and a test that
unmaps half a mapping reads the refusal and then reads its mapping back. This
file is deleted with whichever lands first.

## Owner

`kernel/src/syscall/vm.rs`, `toyos-abi/src/syscall.rs`, `userland/libc`. Nobody holds it.
