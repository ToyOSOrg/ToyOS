---
status: open
kind: defect
opened: 2026-10-04
---

# Every `mmap` walks every region its process holds, under both of its locks

`sys_mmap` (`kernel/src/syscall/vm.rs`) does work in proportion to the regions
the calling process holds, with its process-data lock and its address-space
lock both taken:

- after each placement, `peak_memory` is a sum over all of
  `ProcessData::mmap_regions`;
- `Regions::find_gap` (`kernel/src/vma.rs`) walks the region map from the top
  until a gap fits, which in a top-down fill is every region;
- the FIXED arm's `Regions::occupancy` filters every region below the end of
  the range it asks about (`overlapping`'s `range(..end)`).

`toyos_abi::syscall::MAX_REGIONS` (32,768) bounds each walk, and so the cost;
nothing makes it small. A fill to the bound is quadratic: one process mapping
`PROT_NONE` until refused took 70 s of a TCG guest
(`tests/toyos-rust-tests/src/bin/abuse_mmap_regions.rs`). And a sibling
thread's page fault spins on that address-space lock with interrupts off for
as long as a walk holds it.

Owner: orchestrator. Exit: no walk of a process's regions is taken under
either lock in `sys_mmap` — the peak is a running total, and placement and
occupancy are logarithmic in the region count.
