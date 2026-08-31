---
status: open
kind: defect
opened: 2026-08-31
---

# `SYS_MMAP` serves a request that set bits it does not define

`sys_mmap` (`kernel/src/arch/syscall/vm.rs:19`) reads `MmapProt` and `MmapFlags`
by asking `contains` for the bits it knows — `MmapProt::WRITE` at
`vm.rs:30`, `MmapFlags::FIXED` at `vm.rs:27` — and never asks whether the
caller set anything else. `MmapProt` defines `NONE`/`READ`/`WRITE`
(`toyos-abi/src/syscall.rs:560-562`) and `MmapFlags` defines
`ANONYMOUS`/`PRIVATE`/`FIXED` (`toyos-abi/src/syscall.rs:577-579`); every other
bit of each `u64` is accepted and dropped.

So a caller asking for something this kernel does not do is answered as though
it had asked for something else. `prot = READ | 4` is served as a read-only
mapping, and a caller that believed bit 2 meant "executable" gets a mapping that
is not, with no word said. That is silent degradation on a syscall whose whole
subject is what a page may be used for, and it is the exact shape
`SYS_NAMESPACE_BUILD` stopped having when `NamespaceBuild::flags` gained
`NAMESPACE_FLAGS_KNOWN` (`toyos-abi/src/syscall.rs`,
`kernel/src/arch/syscall/ipc.rs`'s `sys_namespace_build`).

Noticed while looking for an in-tree precedent for refusing an undefined flags
bit. There is none: `SYS_MMAP` is the tree's other flag-word syscall and it
ignores.

## What closing it takes

A `KNOWN` mask beside each of `MmapProt` and `MmapFlags`, and `sys_mmap`
refusing `InvalidArgument` for a word carrying anything outside it — before the
size and address checks, so the answer does not depend on which refusal is
reached first. `userland/libc`'s `mmap` and the std fork's callers must be
audited for a bit they pass today that the kernel does not define; a caller
found passing one is the interesting half of the work, not an obstacle to it.

The instrument is a guest arm: `mmap` with a bit outside the mask must answer
`InvalidArgument`, and the same request without it must succeed, so the refusal
is the bit and not the request.
