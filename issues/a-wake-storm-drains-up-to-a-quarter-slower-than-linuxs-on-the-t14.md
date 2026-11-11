---
status: open
kind: defect
opened: 2026-11-11
---

# A wake storm drains up to a quarter slower than Linux's on the T14

`osbench sched`'s `wake.storm` wakes 1, 4, 16 or 64 parked threads at once
(`tests/guest/osbench/src/wake.rs`). `call` is the waker's call and `drain`
the time until the last woken thread has run.

From the 2026-11-11 scoreboard ([evidence](https://github.com/ToyOSOrg/evidence/tree/d44ebdae1f9f584d7b826e1498dbfd791a8d2339)): ToyOS at main ad5db5a65,
four boots a bench off its NVMe, interleaved with five runs of the bench's
Linux as installed. A cell is the median of the runs' p50s with their
run-to-run range. The AMD bench's Linux timed runs 1 to 3 through the HPET
and runs 4 and 5 through the TSC.

| row | T14 ToyOS | T14 Linux | T14 | AMD ToyOS | AMD Linux | AMD |
|---|---|---|---|---|---|---|
| `wake.storm.drain.w4` | 138,134 ns (15.8%) | 126,715 ns (5.8%) | 1.09x | 55,720 ns (2.7%) | 82,972 ns (15.7%) | 0.67x |
| `wake.storm.drain.w64` | 312,458 ns (4.0%) | 250,576 ns (11.2%) | 1.25x | 239,114 ns (3.5%) | 877,763 ns (34.0%) | 0.27x |

Both T14 rows resolve against ToyOS: each boot read worse than each Linux
run. The other drains and every `call` row are ahead on the T14 (`drain.w16`
175 against 849 µs, `call.w64` 23 against 230 µs), and every storm row is
ahead on the AMD bench. Where a 4-waiter or a 64-waiter drain spends its
time on the T14 is unmeasured;
`issues/a-wake-storm-costs-the-t14-twice-the-amd-benchs-time-per-waiter-and-nothing-says-why.md`
holds the waker's per-waiter cost between the benches.

**Exit**: `wake.storm.drain.*` within Linux's on the T14 in a labelled
current-state comparison, or the difference measured to its parts and
justified.

**Owner**: the scheduler's wake path (`kernel/src/sched/`).
