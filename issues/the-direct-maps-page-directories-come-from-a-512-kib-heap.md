---
status: open
kind: defect
opened: 2026-09-28
---

# The direct map's page directories come from a 512 KiB heap

`paging::init` (`kernel/src/arch/x86_64/paging.rs`) builds the direct map
before `alloc::init`, so every table it takes is a `Box` from the early bump
heap (`EARLY_SIZE`, `kernel/src/mm/alloc.rs`): 512 KiB, 128 pages of 4 KiB. The
direct map takes one page directory per GiB it reaches, plus the root and one
second-level table per 512 GiB. A machine whose memory ends past what those
128 pages hold dies in `paging::init` with `memory allocation of 4096 bytes
failed`, the panic QEMU's edk2 produced when the map reached 1 TiB.

The ceiling is an estimate, not a measurement: 128 pages less the root, one
second-level table, whatever the boot allocated before `mm::init`, and the
alignment padding each growth of `AddressSpace::children` leaves before the
next 4 KiB-aligned table. About 120 GiB of memory.

Owner: orchestrator. Exit condition: no table of the direct map comes from the
early heap, whatever the map's end.
