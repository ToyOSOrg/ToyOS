---
status: open
kind: finding
opened: 2026-08-26
---

# `wake_storm_cost` reds beside other guests on a deliberately loaded dev host

One sighting, dev host, 2026-08-26, in a full 12-wide `cargo test` run with seven
pure-shell spin loops as company on a 14-core machine (load average 53). The
loop was staged to measure something else — a denominator for typed-input loss —
so the load is the instrument here rather than the tree's usual condition.

```
FAIL rs::wake_storm_cost: exit code 101
wake_storm_cost: 16 waiters, 21000 cycles
wake_storm_cost: 64 waiters, 115000 cycles
quadrupling the storm from 16 waiters to 64 took the waker's own cost from
21000 cycles to 115000. `post_n` walks the waiters once and does a constant
amount per claim, so four times the waiters may cost four times the loop and
no more; past this, something in the claim grows with the size of the storm
  FAIL  wake_storm_cost  (585ms)
  ALONE wake_storm_cost: GREEN
```

`cargo run -- --known-red wake_storm_cost` answers **NOT ON THE LIST**, which is
why this file exists: the next reader of this name gets a sighting instead of
nothing.

## Second sighting, hosted shard, 2026-08-26 — the other instrument

Run 32909059602 `guest (1)`, the small-fix batch's pull request (a diff of
tracker closes and unrelated small fixes, none reaching the scheduler or
`post_n`): the same ratio assertion red, 183 of 184 names green beside it. A
hosted shard is four cores running one eight-vCPU guest, so it is loaded by
construction — which means both recorded firings are on oversubscribed hosts
and none on a quiet one. That is half the denominator the section below asks
for; the quiet-host arm is still the missing half.

## What this is and is not evidence about

The assertion is a *ratio* of guest TSC deltas, so a host that deschedules the
vCPU inside the 64-waiter arm and not inside the 16-waiter one moves the ratio
without anything in the kernel changing. That makes host oversubscription a
live alternative to a claim cost that grows with the storm, and this sighting
cannot separate them: it is one observation at one load.

The tree it ran on touches `tests/` only — the harness's typed-input delivery —
and nothing in that diff reaches the scheduler, the wait queues or `post_n`.

## What would settle it

Either arm measured against the other in the same session: the same ratio taken
on a quiet host and on a loaded one, several runs of each, with the host's load
recorded per run. If the ratio moves with the load, the assertion needs a
denominator that host time cannot inflate — a count of claims rather than a span
of cycles. If it does not, the finding is the kernel's and this is a `defect`.
