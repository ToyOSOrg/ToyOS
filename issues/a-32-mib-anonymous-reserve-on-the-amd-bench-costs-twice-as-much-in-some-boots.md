---
status: open
kind: finding
opened: 2026-10-31
---

# A 32 MiB anonymous reserve on the AMD bench costs twice as much in some boots

osbench's `mem.anon.map` times one `vm_reserve` of 32 MiB alone, with no
page touched (`tests/guest/osbench/src/system.rs`, `anon_memory`). On the
AMD bench (Ryzen 7 4800U) its p50 is either about 200 ns/MiB (6.4 µs a
reserve) or 376–388 ns/MiB. Twelve `benchcase` boots of six trees
(2026-10-28 to 2026-10-31, `ToyOSOrg/evidence` branch `pr-1099`
ec19d8d3db4b4ebf01804c738dd17eaebbc202b4, `r4/anon-spawn.txt`) split:

- Nine boots read 198–215, including four of ddc26a3ea, two of b3f9af23a
  and one of f40791833.
- Three boots read 376–388: two of 34409c9c2 and one of f40791833. That
  last is the same tree as one of the fast boots.

So the mode is per boot, not per change. The same boots' `mem.anon.touch`,
`unmap` and `total` show no split. No other row separates the slow boots
from all nine fast ones, apart from `work.build_shaped.j1` at 0.96x
(`r4/slow-boots-comove.txt`). The mode does not follow the settled clock
either: 4291 MHz in a fast boot and 4267 in a slow one. f40791833's slow
boot and its fast one log the same firmware memory map and the same
`pmm:` frame counts.

What a reserve does in the slow mode is not measured. It takes the space's
lock, places the range and builds the range's tables
(`AddressSpace::reserve`, `kernel/src/mm/space.rs`): 1/512 of the range,
here 16 leaf tables
(`issues/user-memory-hot-paths-are-slower-than-2-mib-pages.md`).

A comparison of `mem.anon.map` between two AMD boots can read 1.9x that
neither tree caused, as one did on PR #1099.

**Exit**: a probe that splits a reserve's time into the lock, the placement
and the table build (with its frame allocations and zeroing), in one boot of
each mode, which names the slow mode's cause. Then fix it, or fold the cause
into `space.rs`.

**Owner**: the kernel's address spaces (`kernel/src/mm/space.rs`).
