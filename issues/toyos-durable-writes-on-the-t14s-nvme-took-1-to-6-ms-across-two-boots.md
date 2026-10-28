---
status: open
kind: defect
opened: 2026-10-28
---

# ToyOS's durable writes on the T14's NVMe took 1 to 6 ms across two boots

On the T14 (SK hynix BC501, volatile write cache present, so ToyOS's NVMe
driver issues Flush), two boots of PR #1097, both off the internal NVMe with
DATA on it, read these p50s. Linux is two runs on ext4 on the same NVMe in
each validation:

| row | ToyOS, c8f3fbfb0 | ToyOS, 069cbbeb8 | Linux (both validations) |
|---|---|---|---|
| `persist.fsync_clean` | 3,019,602 ns | 464,505 ns | 22,970 to 23,012 ns |
| `persist.write_4k_fsync` | 6,125,947 ns | 1,099,737 ns | 811,313 to 1,035,997 ns |
| `persist.append_4k_fsync` | 6,182,133 ns | 1,012,768 ns | 796,449 to 1,200,332 ns |
| `persist.seq_write_fsync` | 162 MB/s | 347 MB/s | 527 to 598 MB/s |

No change to the NVMe driver, the block service, the file server or the
DATA format lies between the two heads; the second rebased onto #1095's
boot path, and both booted off the NVMe. Why one boot's durable writes cost
six times the other's is unknown. The clean fsync's remaining 20x, a
superblock write and a Flush where Linux's clean fsync takes 23 µs, is
`issues/a-clean-fsync-on-data-writes-the-superblock-and-flushes-the-disk.md`.

**Exit**: repeated boots on the T14 read ToyOS's `persist.write_4k_fsync`
within Linux's, or the boots that do not are explained by a measured cause.

**Owner**: the NVMe driver (`system/nvme/`) and the DATA volume's commit
(`lib/bcachefs/src/fs.rs`).
