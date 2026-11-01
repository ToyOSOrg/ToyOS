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

The bench's Linux, on ext4 on the same NVMe, read these rows in the runs
interleaved with each set of boots. The c8f3fbfb0 boot had its own
validation's two Linux runs: `write_4k_fsync` 0.81 to 1.01 ms,
`append_4k_fsync` 0.80 to 1.04 ms, `create_fsync_unlink` 0.86 to 0.93 ms, and
`seq_write_fsync` 587 to 598 MB/s. The four 069cbbeb8 boots were interleaved
with five Linux runs: `write_4k_fsync` 0.99 to 1.04 ms, `append_4k_fsync` 0.83
to 1.25 ms, `create_fsync_unlink` 0.94 to 0.98 ms, and `seq_write_fsync` 486 to
600 MB/s. Two of the five ToyOS boots ran slow through all of `persist`.
A third turned slow partway, between `append_4k_fsync` and
`create_fsync_unlink`. No change to the NVMe driver, the block service, the
file server or the DATA format lies between the two heads. The boots' NVMe
and DATA log lines do not tell the slow ones apart, and the cause is
unknown. The figures are evidence branch `scoreboard-2026-10-28`'s and PR
#1097's.

A clean fsync now writes nothing (#1098): two boots of 34409c9c2 read it at
15.9 and 16.1 µs, against Linux's 23 µs (evidence branch `pr-1099` at
ac6b89a, `metal-t14/`). The second of those boots ran slow, its
`write_4k_fsync` at 6.26 ms, and its clean fsync still read 16 µs: the slow
mode appears only where a Flush or a write is owed.

PR #1131's `osbench_disk` boots on the T14 add two readings
(evidence `pr-1131` r1, r3, r4):
- The slow mode took all 4 boots without APST, and 2 of 5 with it.
- The drive is not short of host memory. Its HMPRE and HMMIN are 0, and it
  refuses Feature 0Dh as an invalid field, so it uses no Host Memory Buffer.

**Exit**: repeated boots on the T14 read every durable `persist` row
(`write_4k_fsync`, `append_4k_fsync`, `create_fsync_unlink` and
`seq_write_fsync`) within Linux's, in every boot and for the whole of each
boot's run. Or the slow mode is explained by a measured cause.

**Owner**: the NVMe driver (`system/nvme/`) and the DATA volume's commit
(`lib/bcachefs/src/fs.rs`).
