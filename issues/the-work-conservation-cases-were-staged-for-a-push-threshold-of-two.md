---
status: open
kind: tooling
opened: 2026-10-12
---

# The work-conservation cases were staged for a push threshold of two

`kernel/sim/tests/topology.rs` and `scenarios::group_imbalance` came with the
simulator's I15 under `PUSH_THRESHOLD` 2 and a busy victim that drained a
probe at its quantum's end. At threshold 1, with every ordinary message
kicking a busy target (PR #874), two of their claims stop being shown:

- **I15's bound is not shown to constrain.** Each probe kicks the victim, and
  the extra pass pushes the next sleeper, so the lopsided machine's cursor walk
  ends long before the derivation's one push a round. Over 200 seeds the worst
  stretch is 5 rounds against a bound of 10 on the T14's shape and 6 against
  18 on the AMD bench's ([at `17c40459d`](https://github.com/ToyOSOrg/evidence/blob/d6d8e852b6e7e249e45d6218c252583ee624b03c/r3/sim/scratch-at-head.log)). The bound is still the derivation's worst case; no
  sampled schedule comes near it, so
  `the_push_keeps_an_idle_cpu_beside_work_inside_its_bound` no longer asserts
  that the worst stretch is more than half the bound.
- **`group_imbalance` no longer stages an idle CPU whose CCX holds nothing a
  probe takes.** Each of cpu8's CCX neighbours runs one task and queues one,
  and one queued task is now stealable, so cpu8 can be served from its own
  CCX. A neighbour that looks loaded and has nothing stealable needs load the
  fair band does not express.

Owned by the placement track (`issues/placement-has-no-migration-cost-or-wake-affinity.md`),
which owns the push and the victim choice and the derivation of I15's bound
for them.

**Exit**: the simulator's tests assert that a sampled schedule comes within half of I15's
bound at both benches' shapes, and `group_imbalance` stages CCX load no probe
takes at the shipped threshold.
