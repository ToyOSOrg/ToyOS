---
status: open
kind: defect
opened: 2026-10-12
---

# `interactive_mix` at two CPUs and sixteen hogs crosses invariant I5's derived bound

`kernel/sim`'s invariant I5 bounds the service spread between two equal shares
over one contention window by
`lag_spread + (ΣT + 1) × (QUANTUM + max KernelSection + 2 × RUN_CHUNK)`
(`kernel/sim/src/invariants.rs`, `check_fairness`). `interactive_mix(2, 16)` —
one sleeper thread against a sixteen-thread hog process on two CPUs — crosses
it on `main` as well as with the busy-CPU wake change (PR #874).

Measured with I5's ceiling lifted (`with_fair_allowance(u64::MAX)`), seeds
0..2000 alternating the seeded and PCT streams as `sim/tests/policy.rs` does;
the scratch test that ran it is posted on PR #874:

| tree | runs crossing | worst spread | bound in force there |
|---|---|---|---|
| `main` at `177e72a9e` | 90 / 2000 | 250 ms | 216 ms |
| PR #874 | 196 / 2000 | 254 ms | 217 ms |

The policy suite's forty seeds miss every crossing on `main`, and two of them
(24 and 36) cross with the change, which is why `scenarios::interactive_mix`
now carries a recorded allowance at that shape (`INTERACTIVE_I5_SAMPLE`, the
worst spread plus a quarter, as `FAIRNESS_SAMPLE` does). `Outcome::fair_over_bound`
still reports every crossing of the derived bound.

What a crossing run shows (`interactive_mix(2, 16)`, seed 24, from a step
timeline): the sleeper's first wake waits behind the eight hogs on its CPU,
whose keys are still the spawn's zero, and each later quantum end on that CPU
hands it to a hog whose key went stale when it was queued, while the sleeper
counts as runnable throughout. The spread is the fair band's insertion-time
keys, the same staleness the bound's `ΣT + 1` term carries, past that term.

**Exit**: `interactive_mix(2, 16)` meets I5's derived bound over 2,000 seeds,
and `INTERACTIVE_I5_SAMPLE` is deleted.
