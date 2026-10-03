---
status: open
kind: defect
opened: 2026-10-03
---

# A process's mapping count is bounded only by its address window

Every `mmap` adds a `Region` to the address space's map and an `MmapRegion` to
`ProcessData::mmap_regions` (`sys_mmap`, `kernel/src/syscall/vm.rs`), and
nothing bounds how many one process holds but the placement window
(`kernel/src/vma.rs`): 8 GiB to `STACK_BASE` in 2 MiB pages, 260,094
placements with the 2 MiB guard and 520,188 `FIXED` ones without it. A
`PROT_NONE` mapping pins no physical page, so a loop of them costs the process
nothing and the kernel heap one record in each ledger.

`mmap_regions` is a `Vec` of 40-byte records (`UserAddr`, `usize`,
`Option<PageAlloc>`). The push past 32,768 records grows it to 65,536, an
allocation of 2,621,440 bytes, past `mm::MAX_HEAP_ALLOC` (2,093,056), where
`KernelAllocator::alloc` (`kernel/src/mm/alloc.rs`) asserts: a kernel panic from
one unprivileged process. Below that point the records of many such processes
are kernel heap charged to nobody, and a heap that cannot grow answers `alloc`
with a null, which panics too.

By reading, unmeasured: the 40 bytes are read off the struct, and the counts
are that arithmetic over `vma.rs`'s constants.

Owner: orchestrator. Exit condition: `mmap` refuses by name the mapping past a
per-process bound, and a test that maps `PROT_NONE` until refused reads that
refusal and a live kernel; it reds today on the panic.
