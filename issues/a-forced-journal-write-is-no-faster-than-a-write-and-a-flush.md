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
went as predicted and the time did not. The journal keeps write-plus-Flush,
and the plumbing, which had no other user, did not land.

**What was measured.** `osbench_disk` on both benches, four boots an arm,
interleaved: main with the row (`c9d6eff6f`) against the forced write
(`8a8ff27b9`, closed PR #1202's branch `block-fua`), under rules fixed
before any boot. Each figure is the median of four boots' p50, with the
range over boots. Evidence:
https://github.com/ToyOSOrg/evidence/blob/7efbe1295aa54370b213719b4d4f460e7a103554/ab/README.md

On the AMD bench every boot of each arm agreed, with no overlap:

| row | write and Flush | forced | Linux |
|---|---|---|---|
| `persist.write_4k_fsync` | 2.36 ms (2.24-2.46) | 4.00 ms (4.00-4.01) | 3.58 ms |
| `persist.append_4k_fsync` | 2.41 ms (2.32-2.51) | 4.00 ms (4.00-4.00) | 3.84 ms |
| `persist.create_fsync_unlink` | 2.62 ms (2.54-2.93) | 4.47 ms (4.25-4.64) | 3.32 ms |
| `persist.write_4k_fsync_beside_1m` | 3.03 ms (2.95-3.17) | 4.69 ms (4.63-4.83) | 4.18 ms |
| `persist.seq_write_fsync` | 266 MB/s (243-274) | 212 MB/s (204-223) | 502 MB/s |

On the T14 the drive's slow mode
(`issues/toyos-durable-writes-on-the-t14s-nvme-run-six-times-slower-in-some-boots.md`)
decided every row: slow against slow the arms were equal (5.66-6.24 ms
against 5.78 and 5.92 ms), and the forced arm had no fast boot. Forced
writes in the fast mode were read off a third arm's boots:
`persist.append_4k_fsync` 0.93-1.08 ms, against 0.97 and 1.12 ms for a
write and a Flush.

So: no faster at best, 66% slower at worst.

**The one open question.** The AMD's forced samples sit tightly at 4.00 ms
(one boot: min 3.71, p50 4.01, p90 4.25). Whether that is the drive's cost
of a forced write or a cadence on ToyOS's side was not established: no boot
timed single commands. Anyone who takes this up again starts there, with a
boot that times each command, before building anything.

**Not a reason to retry:** that a commit then issues fewer commands. Nothing
is owed.
