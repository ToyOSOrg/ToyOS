---
status: open
kind: defect
opened: 2026-10-28
---

# A durable write on the T14's NVMe costs ToyOS six times Linux's

On the T14 (SK hynix BC501, volatile write cache present, so ToyOS's NVMe
driver issues Flush), the 2026-10-28 validation of PR #1097 at c8f3fbfb0
read these p50s. ToyOS is one boot, with DATA on the NVMe; Linux is two runs
on ext4 on the same NVMe:

| row | ToyOS | Linux |
|---|---|---|
| `persist.fsync_clean` | 3,019,602 ns | 22,971 and 23,002 ns |
| `persist.write_4k_fsync` | 6,125,947 ns | 811,313 and 1,009,786 ns |
| `persist.append_4k_fsync` | 6,182,133 ns | 796,449 and 1,040,764 ns |
| `persist.seq_write_fsync` | 162 MB/s | 587 and 598 MB/s |

A clean fsync on ToyOS is one superblock write and one Flush
(`issues/a-clean-fsync-on-data-writes-the-superblock-and-flushes-the-disk.md`),
so a Flush on that disk costs ToyOS about 3 ms. Linux's whole write and
fsync, its journal commit included, costs 0.8 to 1.0 ms. On the AMD bench's
Samsung the two systems' durable writes cost about the same (2.9 against 3.0
to 3.6 ms). So the gap is that disk's under ToyOS, not durability's. The
cause is unmeasured. It could be the commit's two flushes, the drive's
power state between requests, or the driver's completion path.

**Exit**: `persist.write_4k_fsync` and `persist.fsync_clean` on the T14
within Linux's, or the remaining difference measured to its parts and
justified, in a labelled current-state comparison.

**Owner**: the NVMe driver (`system/nvme/`) and the DATA volume's commit
(`lib/bcachefs/src/fs.rs`).
