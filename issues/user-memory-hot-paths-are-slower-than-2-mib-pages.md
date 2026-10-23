---
status: open
kind: defect
opened: 2026-10-22
---

# User memory's hot paths are slower than 2 MiB pages

User memory moved from 2 MiB pages filled when mapped to 4 KiB pages filled
at first touch (`kernel/src/mm/space.rs`). Each of these costs more than it
did; the T14's numbers are below:

- **Faults.** About 512 times as many per byte touched. Each takes the
  space's lock, an inventory lookup, two table walks (`reach` reads the leaf
  with `get` and writes it with `set`), and the frame allocator's one lock
  (`issues/per-cpu-scheduler-and-pmm-state-shares-lines-every-cpu-writes.md`)
  and a 4 KiB zeroing.
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
| `mem.first_touch.4k` ns/page | 887 | 11 (2 MiB pages; Linux 927) |
| `mem.map_touch_unmap.4k` ns/page | 909 | 207 |
| `mem.anon.total` ns/MiB | 228,677 | 51,612 (Linux 258,177) |
| `mem.realloc_grow.1m_256m` ms | 116 | 30 (Linux 131 ns, by `mremap`) |
| `fs.tmp.seq_read.1048576` MB/s | 6,060 | 9,425 |
| `fs.tmp.seq_write.1048576` MB/s | 4,297 | 5,889 |
| `fs.tmp.readdir.1000` ms | 1.20 | 0.74 |
| `ipc.reqrep.65536` ns | 47,887 | 38,508 |
| `pipe.throughput.65536` MB/s | 7,021 | 10,902 |

The copies (`fs.tmp`, `ipc`, `pipe`) point at the user-copy path above.

Owner: the memory track. Exit: each path above measured on the T14 at a
change and at its base, and against Linux; a regression is fixed or its cost
accepted by the owner. Cheap remedies when measurement asks: a per-CPU frame
cache, one walk per fault, a count of armed futex waiters that lets a clear
with none skip the buckets, and tables built at the first touch of their span.
