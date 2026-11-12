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

**Owed on metal:** one A/B of main from before and after #1266 (a thread's request follows its bursts), on the T14 and the AMD bench, of osbench's `sched`, `fair` and `time` groups, with a Linux reference timed through the TSC. Main must read the `wake.*` rows faster or unchanged, the `fair.*` share rows within 3%, `time.yield` within 3%, and no scheduler, wake, timer or fairness row slower. The latency change is measured only in the simulator so far.

**Owed on metal:** one A/B of main from before and after #1274 (a wake behind
a busy home goes from its claimant straight to an idle CPU), on the T14 and
the AMD bench. Four boots a side, in the order B,H,H,B,H,B,B,H, where B is
before and H is after. The rows are osbench's `wake.onto_busy`,
`pingpong.*.cross_core`, `ipc.reqrep.*`, `wake.storm.*` and `energy.*`. Main
must read `wake.onto_busy` faster on both benches, with no other of these
rows slower beyond its spread. A second set of the same rows runs with
every CPU busy: there a claimant's scan for an idle CPU finds none, and the
home's own placement scans again, so main must read no row slower there
beyond its spread. A third case is a wake whose home runs a task of another
band with one of the wake's band queued. No `osbench` row stages it yet, and
its reading decides
`issues/a-claimed-wake-ignores-a-queued-task-of-its-band-at-home.md`. With it, the idle-exit latency per sleep depth on both
benches, from the existing probes or the A/B's boots, so that each bench's
`wake.onto_busy` is split between the removed hop and the idle exit. A wake
claimed before its home has switched away from the task still takes both
hops. The speed-up is not yet measured on metal.

**Exit**: `wake.onto_busy` within Linux's on both benches in a labelled
current-state comparison, or the difference measured to its parts and
justified.

**Owner**: the scheduler's wake path (`kernel/src/sched/`).
