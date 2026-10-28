---
status: open
kind: defect
opened: 2026-10-28
---

# A tmpfs metadata call costs ToyOS up to six times Linux's

`osbench files` runs the same operations under each system's kernel memory
filesystem: ToyOS's `/tmp`, which the kernel serves through syscalls, and
Linux's `/dev/shm`, tmpfs through the VFS. No file server is involved.
The 2026-10-28 scoreboard (ToyOS at 069cbbeb8, 4 boots off the internal NVMe against 5 runs of each bench's Linux as installed, interleaved; p50s, with the run-to-run range; evidence branch `scoreboard-2026-10-28`):

| row | T14 ToyOS | T14 Linux | AMD ToyOS | AMD Linux |
|---|---|---|---|---|
| `fs.mem.open_close` | 7,291 ns (7.7%) | 1,114 ns (8.1%) | 6,157 ns (0.8%) | 2,946 ns (48.4%) |
| `fs.mem.stat` | 3,115 ns (5.3%) | 584 ns (2.6%) | 2,616 ns (1.1%) | 1,513 ns (90.7%) |
| `fs.mem.rename` | 7,374 ns (6.3%) | 1,624 ns (2.3%) | 6,282 ns (1.1%) | 4,538 ns (35.7%) |
| `fs.mem.mkdir_rmdir` | 9,011 ns (9.1%) | 2,213 ns (1.5%) | 7,705 ns (3.1%) | 6,702 ns (23.3%) |

On the T14 ToyOS loses all four, 4.1x to 6.5x, and all resolve. On the AMD
it loses `open_close` (2.1x) and `rename` (1.4x), both resolved, and reads
worse unresolved on the other two. That bench's Linux spreads are wide
because two of its five runs timed through the HPET. The listing
(`fs.mem.readdir.1000`) is
`issues/a-listing-still-scans-the-whole-mount.md`'s, and the sequential copies
are `issues/user-memory-hot-paths-are-slower-than-2-mib-pages.md`'s. What a
tmpfs path lookup and metadata call spend their time on is unmeasured.

**Exit**: these four rows within Linux's on both benches in a labelled
current-state comparison, or each remaining difference measured to its parts
and justified.

**Owner**: the kernel's tmpfs and VFS (`kernel/src/tmpfs.rs`,
`kernel/src/vfs.rs`).
