---
status: open
kind: defect
opened: 2026-08-24
---

# I13's margin at 32 CPUs has closed from 10 ms to 2 ms since it was recorded

Found while re-running the fairness sweep for
`issues/kernel/granularity-bound-crossed-at-four-widths.md`, not looked for.

`per-process-fair-split-is-the-policy` records the per-thread split as the half
that does *not* degrade with width, and prices its own confidence on that:
entry criterion 3 says "**The margin at 32 CPUs is 1.2× and trending up** —
10 ms at one CPU to 50 ms at 32 against a 60 ms bound — with nothing measured
above 32, while the scheduler's own staged target is 1–128."

The same command on this tree at `739af0c2`, 500 seeds a width:

```
cargo run --release -p toyos-sched-sim -- measure fairness_storm:<cpus> 500
```

| CPUs | 1 | 2 | 3 | 4 | 6 | 8 | 12 | 16 | 24 | 32 |
|---|---|---|---|---|---|---|---|---|---|---|
| I13 worst, recorded | 10 | 30 | 28 | 28 | 31 | 32 | 35 | 37 | 42 | 50 |
| I13 worst, 2026-08-24 | 10 | 28 | 28 | 28 | 31 | 32 | 35 | 38 | 43 | **58** |

Every width but the last moves by at most 2 ms, and eight of ten move by at most
1 ms. The widest moves by 8, from 50 to 58 against the 60 ms bound — 1.20× margin
to 1.03×. The run is `clean`: `invariant_i13_is_measured_and_holds` still passes,
and I5's whole row reproduces the recorded numbers exactly at nine of ten widths,
which is what says the two runs are the same measurement rather than two
configurations.

**Why this is worth a file rather than a shrug.** The number is the entry
criterion for the per-share-FIFO redesign, and that criterion is about *whether
the answer can be trusted at width*, not about whether the gate is green. At
1.03× the next width up is where the bound is crossed, and 64 and 128 are inside
the scheduler's staged target and have never been measured. It is also possible
this is seed noise at the width where the sweep does the least work per seed
(I13's reach there is 37%, the lowest of the ten) — that is the first thing to
establish, by re-running 32 at a larger seed count and by measuring 64.

Not measured: whether the move is a change in the tree or in the sample. Nobody
has bisected it, and this entry does not claim a regression.

**2026-08-25, promoted to `defect`.** The number is the stated entry criterion
for the per-share-FIFO redesign, and 1.03× against a 60 ms bound is not a margin
anyone may enter on — that makes it work owed rather than something noticed.
Two runs answer it and both are cheap: `fairness_storm:32` at a seed count well
above 500, which decides whether 58 is the tree or the sample, and
`fairness_storm:64`, which is inside the scheduler's staged 1–128 target and has
never been run. Owed by whoever picks up the per-share-FIFO work, before
starting it rather than after.
