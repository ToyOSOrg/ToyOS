---
status: open
kind: defect
opened: 2026-10-28
---

# A yield costs ToyOS up to 3.4 times Linux's

std's `thread::yield_now` on ToyOS is `SYS_YIELD` (`sdk/std/sys/thread.rs`),
which gives the CPU to another runnable thread of the caller's band, and
`osbench`'s `time.yield` times it with no other thread to run
(`tests/guest/osbench/src/time.rs`). The 2026-10-28 scoreboard recorded the
row unsupported on ToyOS, where `yield_now` was then a spin-loop hint.

From the 2026-11-11 scoreboard ([evidence](https://github.com/ToyOSOrg/evidence/tree/d44ebdae1f9f584d7b826e1498dbfd791a8d2339)): ToyOS at main ad5db5a65,
four boots a bench off its NVMe, interleaved with five runs of the bench's
Linux as installed. A cell is the median of the runs' p50s with their
run-to-run range. The AMD bench's Linux timed runs 1 to 3 through the HPET
and runs 4 and 5 through the TSC.

| row | T14 ToyOS | T14 Linux | T14 | AMD ToyOS | AMD Linux | AMD |
|---|---|---|---|---|---|---|
| `time.yield` | 651,614 ps (0.5%) | 190,906 ps (2.1%) | 3.41x | 622,208 ps (4.2%) | 551,970 ps (1.3%) | 1.13x |

Both resolve against ToyOS. The same boots' `syscall.null` reads 68 ns on
the T14 and 49 ns on the AMD bench, so a yield with nothing to yield to costs
some 580 ns more than an empty system call on both. What that is spent on is
unmeasured.

**Exit**: `time.yield` within Linux's on both benches in a labelled
current-state comparison, or the difference measured to its parts and
justified.

**Owner**: the scheduler's yield (`kernel/src/sched/`).
