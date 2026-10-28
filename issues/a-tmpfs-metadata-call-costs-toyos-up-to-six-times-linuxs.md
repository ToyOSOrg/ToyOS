---
status: open
kind: defect
opened: 2026-10-28
---

# A tmpfs metadata call costs ToyOS up to six times Linux's

`osbench files` runs the same operations under each system's memory
filesystem: ToyOS's `/tmp`, which the kernel serves through syscalls today,
and Linux's `/dev/shm`, tmpfs through the VFS.

From the 2026-10-28 scoreboard (evidence branch `scoreboard-2026-10-28`):
ToyOS at 069cbbeb8, four boots off the internal NVMe, against five runs of
each bench's Linux as installed, interleaved. A cell is the p50 with its
run-to-run range. The AMD's Linux runs 1 to 3 timed through the TSC and 4
and 5 through the HPET, at 1.2 µs a timestamp.

| row | T14 ToyOS | T14 Linux | AMD ToyOS | AMD Linux |
|---|---|---|---|---|
| `fs.mem.open_close` | 7,291 ns (7.7%) | 1,114 ns (8.1%) | 6,157 ns (0.8%) | 2,946 ns (48.4%) |
| `fs.mem.stat` | 3,115 ns (5.3%) | 584 ns (2.6%) | 2,616 ns (1.1%) | 1,513 ns (90.7%) |
| `fs.mem.rename` | 7,374 ns (6.3%) | 1,624 ns (2.3%) | 6,282 ns (1.1%) | 4,538 ns (35.7%) |
| `fs.mem.mkdir_rmdir` | 9,011 ns (9.1%) | 2,213 ns (1.5%) | 7,705 ns (3.1%) | 6,702 ns (23.3%) |
| `fs.mem.tree.100` | 1,505,195 ns (4.1%) | 548,925 ns (1.1%) | 1,384,280 ns (1.3%) | 1,457,995 ns (2.6%) |
| `fs.mem.create_write_unlink` | 12,832 ns (4.7%) | 4,715 ns (2.1%) | 10,801 ns (0.6%) | 12,743 ns (13.7%) |
| `fs.mem.write_4k_at` | 1,237 ns (4.0%) | 625 ns (5.0%) | 1,563 ns (7.7%) | 1,823 ns (61.0%) |
| `fs.mem.read_4k_at` | 1,222 ns (2.3%) | 710 ns (4.5%) | 1,422 ns (3.5%) | 1,403 ns (70.8%) |

On the T14 ToyOS loses all eight, 1.7x to 6.5x, each resolved. On the AMD it
loses `open_close` (2.1x) and `rename` (1.4x), both resolved, wins
`tree.100`, `create_write_unlink` and `write_4k_at`, and is unresolved on the
other three. There the HPET explains Linux's spread: its TSC runs read
`open_close` at 2.9 µs and its HPET runs at 4.3 µs. The listing
(`fs.mem.readdir.1000`) is `issues/a-listing-still-scans-the-whole-mount.md`'s,
and the sequential copies are
`issues/user-memory-hot-paths-are-slower-than-2-mib-pages.md`'s.

**The kernel's tmpfs is not where this is fixed.** Step 9 of
`issues/the-kernel-is-small-interrupts-post-and-threads-wait.md`, under the
owner's rulings of 2026-09-25 and 2026-10-03, deletes `tmpfs.rs`,
`fs_rename.rs`, `file_cache.rs` and `vfs.rs`, and moves `/tmp` to
userland per session. An `fs.mem` call then becomes a file server request,
which costs 3 to 12 times a kernel file call today
(`issues/a-file-server-request-costs-three-to-twelve-times-a-kernel-file-call.md`).
So the move makes this gap larger, unless the file server's round trip
shrinks first.

**Exit**: once `/tmp` is served from userland, these eight rows within
Linux's on both benches in a labelled current-state comparison, or each
remaining difference measured to its parts and justified.

**Owner**: whatever serves `/tmp` through step 9's migration. Until then,
nothing in the kernel's tmpfs is owed this work.
