---
status: open
kind: defect
opened: 2026-11-05
---

# A large spawn on the AMD bench is a seventh slower than main's

`osbench`'s `proc.spawn_wait.large` spawns a 32 MiB program from a served
file. The spawner reads the image through `SYS_PAGED_READ` into fresh shared
memory. #1150 copies it a frame at a time through the reader's own mapping
(`arch::uaccess`, `PagerObject::read`). Main copied it through the kernel's
direct map, reaching and pinning each page as it went. The copy's fault on
the shared memory maps its 64 KiB run (`kernel::vm::around`); the figures
below were read when a pass of its own faulted each chunk of 16 frames in
first.

On the AMD bench (metal round 8; evidence `ToyOSOrg/evidence`, branch
`pr-1150`, r15):
- **The row:** 12.79 and 12.75 ms against main's 11.00 and 11.53, worse in
  both pairs.
- **The copy:** 760 ns per page against 685 for main's whole reach, pin and
  copy; with 8192 pages a spawn, that is 0.6 ms.
- **The populate:** that pass added 48 ns per page, 0.4 ms a spawn.
- **Unaccounted:** about 0.5 ms of the 1.5 ms.

Before #1150's accessed-and-dirty change to x86-64 user leaves the copy cost
1871 ns per page and the row was 96% worse. On the T14 the row is now 4%
better than main's.

**Hypothesis, unmeasured:** the remaining copy cost is translating each
destination page through the 4 KiB user mapping, where main's direct map uses
huge pages. No performance counter has been read.

**Exit:** the row within main's run-to-run range on the AMD bench, or the
per-page difference measured by counter (TLB misses and walk cycles, against
main) and recorded here as the cost of copying through the user mapping that
#1150's design accepts.

- **Owed on metal:** #1245 copies the image through faults that map 64 KiB
  runs of the shared memory, where a populate pass ran first. In the A/B of
  main from before and after #1245
  (`issues/user-memory-hot-paths-are-slower-than-2-mib-pages.md`), on both
  benches, `proc.spawn_wait.large` and `ab.fs.memory.shm.16777216`, the same
  copy without the spawn, must not regress: no rise in all four paired boots
  with a median ratio above 1.03.
