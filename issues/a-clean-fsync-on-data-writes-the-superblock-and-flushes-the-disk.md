---
status: open
kind: defect
opened: 2026-10-28
---

# A clean fsync on DATA writes the superblock and flushes the disk

An fsync of a file on DATA syncs the whole volume
(`issues/an-fsync-of-a-served-file-syncs-its-whole-volume.md`), and the
volume's sync, `bcachefs::Mounted::sync` (`lib/bcachefs/src/fs.rs`), commits
unconditionally. It flushes the cache, writes the superblock, and flushes
again. The file server's cache issues a device flush only where it owes one,
after a write it made. The superblock write makes one owed every time, so an
fsync with nothing new to make durable still costs a block write and an NVMe
Flush.

Measured on the AMD bench (2026-10-28, `osbench persist` at PR #1097's
c8f3fbfb0, DATA on its designated NVMe partition): `persist.fsync_clean`
p50 1,233,821 ns under ToyOS, against 56,649 and 69,703 ns under the
bench's Linux on the same NVMe. That is about the cost of one Flush on that
disk, and `persist.write_4k_fsync`, which owes one, reads 2.9 ms on ToyOS
and 3.0 to 3.6 ms on Linux.

**Exit**: a sync of a volume with nothing uncommitted writes and flushes
nothing, shown by counting the requests a clean sync issues on a host disk.
`persist.fsync_clean` on the AMD bench then reads within the file server's
round trip, tens of µs and not a Flush's ms.

**Owner**: the DATA format (`lib/bcachefs/src/fs.rs`) and the file server's
DATA volume (`system/fileserver/src/data.rs`).
