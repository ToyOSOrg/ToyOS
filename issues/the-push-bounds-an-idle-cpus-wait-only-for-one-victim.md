---
status: open
kind: finding
opened: 2026-10-12
---

# The push bounds an idle CPU's wait only for one victim

The simulator's invariant I15 (`kernel/sim/src/invariants.rs`,
`work_conservation_bound`) holds the shipped balance path to `cpus + 2`
protocol rounds of an idle CPU beside stealable work. The bound is derived for
one victim: that victim passes once a round and its push walks a cursor over
every sleeper, so the idle CPU is rung within `cpus − 1` of its passes.

With several victims nothing derives a bound. Each victim walks its own cursor,
so a sleeper can be passed over by each in turn, and a thief probes the CPU
publishing the most surplus, so a probe sent to a victim that runs dry before it
answers leaves the thief waiting for a ring again. Neither is shown to happen:
over the suite at every simulator shape, and over storm and interactive
workloads at 8, 16 and 64 CPUs, the worst stretch measured is 17 rounds against
a bound of 66. It is the derivation that is missing, not a schedule that
crosses it.

Owned by the placement track (`issues/placement-has-no-migration-cost-or-wake-affinity.md`),
which owns the push and the victim choice.

Exit: I15's bound is derived for the shipped balance protocol over any number
of victims.
