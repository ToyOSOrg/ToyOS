---
status: open
kind: defect
opened: 2026-08-01
---

# The scheduler crosses its own derived granularity bound at four of ten widths

Distinct from `per-process-fair-split-is-the-policy`, and deliberately not merged
with it. That one says fairness degrades as the machine widens. **This one says
the shipped scheduler exceeds a limit its own design implies** — a different and
sharper statement.

The bound is derived from granularities the policy itself picked:
`lag_spread + (ΣT_i + 1) × (QUANTUM + max KernelSection + 2 × RUN_CHUNK)`
(`toyos-sched/sim/src/invariants.rs:695-697`).

**Re-measured 2026-08-24 at `739af0c2`**, the whole sweep, 500 seeds a width:

```
cargo run --release -p toyos-sched-sim -- measure fairness_storm:<cpus> 500
```

| CPUs | 1 | 2 | 3 | 4 | 6 | 8 | 12 | 16 | 24 | 32 |
|---|---|---|---|---|---|---|---|---|---|---|
| I5 worst spread (ms) | 30 | 84 | 125 | 198 | 324 | 418 | 634 | 720 | 1056 | 1386 |
| crossing (ms) | — | — | — | **112** | **324** | **418** | **634** | — | — | — |

Still four widths and still the same four. The crossing row is
`Outcome::fair_over_bound`: the widest spread seen in a window where `spread`
exceeded the **derived** bound, which is not the same quantity as the amount by
which it exceeded it, and not the same quantity as the worst spread either —
at 4 CPUs the worst window is 198 ms and the worst *crossing* window is 112 ms.
The entry read "crossed ... by 116, 324, 418 and 634 ms" and that was wrong in
kind as well as in one figure; the 4-CPU number is 112 ms today.

**The gate handles this honestly rather than hiding it**, which is the part worth
preserving. It reds on `max(derived, recorded allowance)`
(`invariants.rs:723`), so a sampled scenario is gated on not regressing — but
`fair_over_bound` records every crossing of the *derived* bound regardless, and
the sweep prints `N ns PAST THE DERIVED BOUND on the recorded allowance`
(`sweep.rs:150-156`). **The allowance cannot quietly become the standard**, which
is the failure mode of every temporary baseline and the reason most of them end
up permanent.
