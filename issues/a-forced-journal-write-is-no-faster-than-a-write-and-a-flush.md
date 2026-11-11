---
status: none
kind: rejected
opened: 2026-11-11
---

# A forced journal write is no faster than a write and a Flush, and slower on the AMD bench

**Measured and declined on 2026-11-11** (the orchestrator's ruling on PR
#1202). The bcachefs journal writes each entry after one flush and follows
it with another. PR #1202 carried Force Unit Access through the block
protocol, the NVMe driver and the USB mass-storage driver, and wrote the
entry forced instead, so that a commit cost one Flush fewer. The commands
went as predicted and the time did not: every durable row was 55% to 71%
slower on the AMD bench and no faster on the T14. One bulk row went the
other way on the T14 and is stated below; a mixed bulk result does not pay
for that loss on every durable row of one bench, and that is the reason for
the ruling. The journal keeps write-plus-Flush, and the plumbing, which had
no other user, did not land.

**What was measured.** `osbench_disk` on both benches, four boots an arm,
interleaved: main with the row (`c9d6eff6f`) against the forced write
(`8a8ff27b9`, closed PR #1202's branch `block-fua`), under rules fixed
before any boot. Each ToyOS figure is the median of four boots' p50, with
the range over boots; each Linux figure is the median of three runs' p50.
Evidence:
https://github.com/ToyOSOrg/evidence/blob/7efbe1295aa54370b213719b4d4f460e7a103554/ab/README.md

**The durable rows.** On the AMD bench every boot of each arm agreed, with
no overlap:

| row | write and Flush | forced | Linux |
|---|---|---|---|
| `persist.write_4k_fsync` | 2.36 ms (2.24-2.46) | 4.00 ms (4.00-4.01) | 3.58 ms |
| `persist.append_4k_fsync` | 2.41 ms (2.32-2.51) | 4.00 ms (4.00-4.00) | 3.84 ms |
| `persist.create_fsync_unlink` | 2.62 ms (2.54-2.93) | 4.47 ms (4.25-4.64) | 3.32 ms |
| `persist.write_4k_fsync_beside_1m` | 3.03 ms (2.95-3.17) | 4.69 ms (4.63-4.83) | 4.18 ms |
| `persist.seq_write_fsync` | 266 MB/s (243-274) | 212 MB/s (204-223) | 502 MB/s |

On the T14 the drive's slow mode
(`issues/toyos-durable-writes-on-the-t14s-nvme-run-six-times-slower-in-some-boots.md`)
decided every durable row: slow against slow the arms were equal
(5.66-6.24 ms against 5.78 and 5.92 ms), and the forced arm had no fast
boot. Forced writes in the fast mode were read off a third arm's boots,
`24f5de778`: PR #1131 on the forced write, which also set Autonomous Power
State Transitions, so this comparison is forced-with-APST against
write-and-Flush without. Its `persist.append_4k_fsync` read 0.93-1.08 ms,
against 0.97 and 1.12 ms.

So, on the durable rows: no faster at best, 71% slower at worst
(`persist.create_fsync_unlink` on the AMD bench).

**The row that points the other way.** `fs.disk.seq_write.1048576`, 64 MiB
written in 1 MiB calls with no `fsync`, where the journal's commits fall
among the cache's write-back:

| bench | write and Flush, each boot | forced, each boot | |
|---|---|---|---|
| T14 | 394, 386, 362, 394 MB/s | 472, 498, 496, 460 MB/s | 24% faster forced, no overlap, in slow and fast boots alike |
| AMD | 472, 458, 463, 471 MB/s | 404, 406, 399, 406 MB/s | 13% slower forced, no overlap |

Nothing explains the T14's gain; it was not looked into. On the AMD bench
`fs.disk.tree.100` was also 38% slower forced and
`persist.seq_write_fsync` 20% slower, both with no overlap.

**The one open question.** The AMD's forced samples sit tightly at 4.00 ms
(one boot: min 3.71, p50 4.01, p90 4.25). Whether that is the drive's cost
of a forced write or a cadence on ToyOS's side was not established: no boot
timed single commands. Anyone who takes this up again starts there, with a
boot that times each command, before building anything.

**Not a reason to retry:** that a commit then issues fewer commands. The
T14's bulk-write gain is a reason to look at that row, not at the journal's
durable path. Nothing is owed.
