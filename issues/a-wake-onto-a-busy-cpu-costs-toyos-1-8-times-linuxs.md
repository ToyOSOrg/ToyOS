---
status: open
kind: defect
opened: 2026-11-11
---

# A wake onto a busy CPU costs ToyOS 1.8 times Linux's

`osbench sched`'s `wake.onto_busy` wakes a thread parked on a CPU a spinner
holds, beside idle CPUs, and times it to running
(`tests/guest/osbench/src/wake.rs`).

From the 2026-11-11 scoreboard ([evidence](https://github.com/ToyOSOrg/evidence/tree/d44ebdae1f9f584d7b826e1498dbfd791a8d2339)): ToyOS at main ad5db5a65,
four boots a bench off its NVMe, interleaved with five runs of the bench's
Linux as installed. A cell is the median of the runs' p50s with their
run-to-run range. The AMD bench's Linux timed runs 1 to 3 through the HPET
and runs 4 and 5 through the TSC.

| row | T14 ToyOS | T14 Linux | T14 | AMD ToyOS | AMD Linux | AMD | AMD, TSC runs |
|---|---|---|---|---|---|---|---|
| `wake.onto_busy` | 4,444 ns (2.2%) | 2,553 ns (3.0%) | 1.74x | 18,335 ns (1.2%) | 10,197 ns (29.1%) | 1.80x | 2.16x |

Both resolve against ToyOS. The 2026-10-28 scoreboard read 1.72x and 1.91x.
ToyOS's own figure on the AMD bench is four times its T14 figure, where
Linux's is four times too. What the wake spends its time on is unmeasured.
The metal row `wake_onto_busy` bounds the same shape for ToyOS alone, and
`issues/an-ordinary-wake-posted-on-its-own-cpu-waits-for-that-cpus-next-pass.md`
and `issues/no-row-holds-a-wake-beside-seven-spinners-to-100-us.md` record
what is known of busy targets; neither compares with Linux.

**Exit**: `wake.onto_busy` within Linux's on both benches in a labelled
current-state comparison, or the difference measured to its parts and
justified.

**Owner**: the scheduler's wake path (`kernel/src/sched/`).
