---
status: open
kind: tooling
opened: 2026-09-13
---

# The pass-cost gate's CI sample has been exceeded twice in eight days

`sched_check_build`'s cost half on CI judges a boot's scheduler-pass
distribution against a recorded sample: sixteen CI runs, thirty-two CPU-runs,
2026-08-17 to 18, with zero passes over 200 000 ns and a 90th percentile of
32 768 ns (`tests/common/passcost.rs`). The line is one bucket above that
sample's worst 90th percentile.

CI has now crossed it twice on branches that touch no scheduler code, each
time green alone in the same job:

- `ci` run 33973213660, `guest (8)`, 2026-09-05, on `pkg-install-file`:
  1956 passes, p90 < 262144 ns, 366 over the budget.
- `ci` run 34768854940, `guest (11)`, 2026-09-13, the merge queue's run for
  PR #448 (`host-bridge-abi`, the sysroot half of the host-bridge windows;
  it moves no scheduler code): 3919 passes, p50 < 131072 ns,
  p90 < 262144 ns, p99 < 262144 ns, max 2487630 ns, 51 over the budget.
  The red dequeued the pull request and disarmed its auto-merge.

Two sightings of a whole-distribution shift on a population the sample said
had none is not the sample returning; it is the population moving. Either
CI's runners changed under the sample (the August runs were one Azure SKU;
nothing records which SKU each shard lands on), or the tree's pass cost
moved in a way the dev host cannot see. The gate cannot tell, so every
further crossing costs a landing an hour and answers nothing.

**Exit condition.** The sample is re-recorded from the CI runs since
2026-09-01 — every `guest` shard that ran this name, with the runner's
reported CPU model beside each reading — and the line re-derived from it
with the same rule; if the readings split by runner model, the gate names
the model it judges and refuses to judge the rest. Until then, a CI red
under this name is this file and the redlist row, and the landing it
dequeues is re-queued once, not re-run until green.
