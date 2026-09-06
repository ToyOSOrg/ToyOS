---
status: open
kind: defect
opened: 2026-09-06
---

# A shard's timestamps run backwards at seq 517, and sometimes reds `sched_stress`

The log gate reports one record per full-tier run whose timestamp is behind the
record before it *in its own shard*, which the gate states is impossible:

```
[log] unbracketed: log-gate: FAILED: cpu6 seq 517 is stamped 736451308 ns,
behind the 739564366 ns of the record before it — within a shard the sequence
order is the timestamp order, and `emit` stamps inside the same bracket it
reserves in
```

Three full-tier runs on this host, one of them on `origin/metal` (`b3c314cf`)
with no working-tree diff at all:

| run | tree | shard | inversion | outcome |
|---|---|---|---|---|
| 1 | `t14-run4` | cpu5 | 650224011 behind 651750439 ns (1.5 ms) | `FAIL sched_stress`, 324/325 |
| 2 | `b3c314cf` (base) | cpu6 | 742053639 behind 743716169 ns (1.7 ms) | 325/325 green |
| 3 | `t14-run4` | cpu6 | 736451308 behind 739564366 ns (3.1 ms) | 325/325 green |

So it is **not** the `t14-run4` diff — it fires on the untouched base — and it is
not the retired `sched_stress` entry in `src/redlist.rs` either, whose signature
is a `BTreeMap` panic at `navigate.rs:161`. It is a third thing, and the only
reason it is not a permanent red is that the boot it lands in is usually not one
a test is judging.

Two things about it are stable across all three runs and worth an eye: it is
always **`seq 517`**, and it is always an AP's shard, never cpu0's. A constant
sequence number across three runs and two different CPUs says the record's
position in the shard is what selects it, not the wall time or the CPU.

The gate's own sentence names the invariant that is broken: `emit` stamps
`record.at_ns` and reserves the shard slot inside one `LogCommitGuard` bracket,
so a later `seq` in a shard cannot carry an earlier stamp unless either the
stamp or the reservation escapes that bracket, or `clock::nanos_since_boot`
reads backwards on that CPU. The last of those is the cheapest to check first
and overlaps `issues/kernel/ap-tsc-trail-is-assumed-and-never-checked.md`.

**Exit condition**: the inversion explained and gone — either a `seq 517` that
holds its bracket, or a demonstration that the AP's TSC is what moved, priced
against that issue. Until then a `sched_stress` red carrying this line is this
defect and not the author's diff.
