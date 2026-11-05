---
status: open
kind: defect
opened: 2026-11-05
---

# A large spawn on the AMD bench is a seventh slower than main's

`osbench`'s `proc.spawn_wait.large` spawns a 32 MiB program from a served
file. The spawner reads the image through `SYS_PAGED_READ` into fresh shared
memory. #1150 copies it a frame at a time through the reader's own mapping
(`arch::uaccess`), after faulting each chunk of 16 frames in
(`PagerObject::read`, `UserBytesMut::fault_in`). Main copied it through the
kernel's direct map, reaching and pinning each page as it went.

On the AMD bench (metal round 8; evidence `ToyOSOrg/evidence`, branch
`pr-1150`, r15):
- **The row:** 12.79 and 12.75 ms against main's 11.00 and 11.53, worse in
  both pairs.
- **The copy:** 760 ns per page against 685 for main's whole reach, pin and
  copy; with 8192 pages a spawn, that is 0.6 ms.
- **The populate:** `fault_in` adds 48 ns per page in a pass of its own,
  0.4 ms a spawn.
- **Unaccounted:** about 0.5 ms of the 1.5 ms.

Before #1150's accessed-and-dirty change to x86-64 user leaves the copy cost
1871 ns per page and the row was 96% worse. On the T14 the row is now 4%
better than main's.

**Hypothesis, unmeasured:** the remaining copy cost is translating each
destination page through the 4 KiB user mapping, where main's direct map uses
huge pages. The separate populate pass touches each page's leaf a second time.
No performance counter has been read.

**Exit:** the row within main's run-to-run range on the AMD bench, or the
per-page difference measured by counter (TLB misses and walk cycles, against
main) and recorded here as the cost of copying through the user mapping that
#1150's design accepts.
