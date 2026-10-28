---
status: open
kind: defect
opened: 2026-10-22
---

# User memory's hot paths are slower than 2 MiB pages

User memory moved from 2 MiB pages filled when mapped to 4 KiB pages filled
at first touch (`kernel/src/mm/space.rs`). Each of these costs more than it
did; the T14's numbers are below:

- **Faults.** A touch beside a populated page of a private range fills its
  aligned 64 KiB run (`kernel::vm::around`), so memory touched in order takes
  a fault per 16 pages; a lone touch is still one fault per page. Each fault
  takes the space's lock, an inventory lookup, one table walk and, per frame,
  the frame allocator's one lock
  (`issues/per-cpu-scheduler-and-pmm-state-shares-lines-every-cpu-writes.md`)
  and a 4 KiB zeroing. Probed on the T14 at `b48c7dd9d` (scratch probe, one
  frame per fault): about 345 ns from the trap handler's entry to its return —
  47 to take the lock, 203 under it (36 the allocator, 145 the zeroing of a
  cold frame, the rest the lookup and walks), 52 after — and, by difference
  from the bench's own 1,003 ns under the probe, some 650 ns outside it: the
  exception's delivery and return and the touch that follows. With runs, per
  page that leaves the frame (about 36 + 145 ns) and the memory traffic of
  writing zeroed pages back, which the probe cannot attribute; 2 MiB pages
  or less traffic are what is left to win.

  Runs cost, each owed a fix or the owner's acceptance:
  - **Untouched pages resident.** A run draws up to 15 committed pages
    nothing has touched, zeroing each; resident and peak count them, and a
    thread's stack is resident in 64 KiB once its second page is touched.
  - **`mem.anon.unmap`**, +9% p50 at `a75abd3f6` in two pairs; not
    reproduced at `6d826e313` (4,421 to 4,303 ns/MiB).
  - **A lone fault** costs about 19 ns more: `mem.first_touch.2m`, one page
    per 2 MiB, is 684 to 703 ns/page at `6d826e313` (681 to 706 at
    `a75abd3f6`), though it now takes one walk of the tables
    (`UserTables::leaf_table`); cause not found. Accepted as a measured
    tradeoff by the coordinator on PR #1055, under the owner's delegation
    of implementation tradeoffs (2026-10-23), against about 380 ns saved per page touched in order.
    That row now faults every 4 KiB (PR #1079, so both systems fault the
    same bytes), so no osbench row measures a lone fault any more.
  - **Spawn.** `proc.spawn_wait_loop.100` rose 2.3% at `a75abd3f6` and is
    within 1.5% at `6d826e313`.
- **Frees.** A clear walks the 64 futex buckets once per table of leaves that
  held an owned frame (at most 512 frames each), whether or not anything waits.
- **Commits.** A committed range's tables are built when it is made, under the
  space's lock: 1/512 of the range.
- **Copies.** One round trip of the space's lock per 16 pages, plus an
  inventory lookup and a table walk per page.
- **Spawn.** A segment is copied a page at a time at first touch, so while a
  child lives the image is held twice: once by the spawner's object or the
  `/tmp` file's snapshot, which the child's ranges keep alive, and once
  promised to the child, until shared text removes one. A snapshot takes a
  reference to each resident page of the file under the file cache's lock,
  and 16 bytes a page, at the spawn.

- **Shootdowns.** A change to a present leaf reaches the CPUs running the
  space, each of which flushes every user translation, and a CPU entering a
  space that changed flushes them all too
  (`issues/a-tlb-shootdown-reaches-only-the-cpus-that-ran-the-space.md`).

osbench measures faults (`mem.first_touch`, `mem.anon`), small maps
(`mem.map_unmap_4k`), partial unmaps and protection changes against Linux on
one machine.

On the T14 at 53049d55c against its base 35e930136 (osbench p50, one run
each), what is still slower than the base:

| row | head | base |
|---|---|---|
| `mem.first_touch.4k` ns/page | 887; 508 with runs | 11 (2 MiB pages; Linux 927) |
| `mem.map_touch_unmap.4k` ns/page | 909; 532 with runs | 207 |
| `mem.anon.total` ns/MiB | 228,677; 132,877 with runs | 51,612 (Linux 258,177) |
| `fs.tmp.seq_read.1048576` MB/s | 6,060 | 9,425 |
| `fs.tmp.seq_write.1048576` MB/s | 4,297 | 5,889 |
| `fs.tmp.readdir.1000` ms | 1.20 | 0.74 |
| `ipc.reqrep.65536` ns | 47,887 | 38,508 |
| `pipe.throughput.65536` MB/s | 7,021 | 10,902 |

The copies (`fs.tmp`, `ipc`, `pipe`) point at the user-copy path above.
"With runs" is `a75abd3f6` against `b48c7dd9d` on the T14, one run each;
`6d826e313` gave 500, 521 and 132,434 for the first three rows.

`mem.bw.copy` streams two 64 MiB buffers it asks to be huge. In the
2026-10-28 scoreboard (evidence branch `scoreboard-2026-10-28`) Linux gave
131,072 KiB of huge pages and ToyOS none; ToyOS read 0.95 of Linux's on the
T14 (6,651 against 6,992 MB/s) and 0.98 on the AMD (8,352 against 8,550),
each resolved worse.

`mem.realloc_grow` no longer copies. std and libc map a block of 256 KiB or
more on its own (`sdk/libc/src/mapped.rs`) and grow it by `vm_remap`, which
moves pages. On the T14 at 204243e7f, `1m_256m` took 45.9 us, its last
doubling 10.0 us. Linux, with osbench's block made to escape, took 86.5 and
20.0 us. Its earlier 131 ns came from a build that never allocated. The owner
accepted on 2026-10-27 that `mem.malloc_churn.1048576` loses heap reuse for
this (T14: 29,505 to 194 kops/s; Linux 215) until ToyOS's own allocator
brings it back.

Owner: the memory track. Exit: each path above measured on the T14 at a
change and at its base, and against Linux; a regression is fixed or its cost
accepted by the owner, or by the coordinator for an implementation tradeoff,
which the owner delegated on 2026-10-23. Cheap remedies when measurement asks: a
per-CPU frame cache, a count of armed futex waiters that lets a clear
with none skip the buckets, and tables built at the first touch of their span.
