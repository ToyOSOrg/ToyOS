---
status: open
kind: defect
opened: 2026-10-28
---

# ToyOS's durable writes on the T14's NVMe run six times slower in some boots

On the T14 (SK hynix BC501, volatile write cache present, so ToyOS's NVMe
driver issues Flush), five boots of PR #1097's storage code on 2026-10-28,
all off the internal NVMe with DATA on it, read these p50s:

| boot | `fsync_clean` | `write_4k_fsync` | `append_4k_fsync` | `create_fsync_unlink` | `seq_write_fsync` |
|---|---|---|---|---|---|
| c8f3fbfb0 (16:08) | 3.02 ms | 6.13 ms | 6.18 ms | 6.87 ms | 162 MB/s |
| 069cbbeb8 (16:47) | 0.46 ms | 1.10 ms | 1.01 ms | 1.17 ms | 347 MB/s |
| 069cbbeb8 (17:31) | 3.42 ms | 6.65 ms | 6.85 ms | 5.05 ms | 149 MB/s |
| 069cbbeb8 (17:39) | 0.50 ms | 1.08 ms | 1.04 ms | 7.10 ms | 167 MB/s |
| 069cbbeb8 (17:48) | 0.58 ms | 1.08 ms | 1.09 ms | 1.23 ms | 311 MB/s |

The bench's Linux, five runs interleaved with these on ext4 on the same
NVMe, read `write_4k_fsync` and `append_4k_fsync` at 0.81 to 1.20 ms in
every run. Two of the five ToyOS boots ran slow through all of `persist`.
A third turned slow partway, between `append_4k_fsync` and
`create_fsync_unlink`. No change to the NVMe driver, the block service, the
file server or the DATA format lies between the two heads. The boots' NVMe
and DATA log lines do not tell the slow ones apart, and the cause is
unknown. A fast boot's clean fsync, a superblock write and a Flush, takes
0.46 to 0.58 ms where Linux's takes 23 µs
(`issues/a-clean-fsync-on-data-writes-the-superblock-and-flushes-the-disk.md`).
The figures are evidence branch `scoreboard-2026-10-28`'s and PR #1097's.

**Exit**: repeated boots on the T14 read ToyOS's `persist.write_4k_fsync`
within Linux's every time, or the boots that do not are explained by a
measured cause.

**Owner**: the NVMe driver (`system/nvme/`) and the DATA volume's commit
(`lib/bcachefs/src/fs.rs`).
