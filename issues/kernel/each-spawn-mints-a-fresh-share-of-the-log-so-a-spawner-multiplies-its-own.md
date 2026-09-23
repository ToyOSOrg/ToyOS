---
status: open
kind: defect
opened: 2026-09-23
---

# Each spawn mints a fresh share of the log, so a program that spawns speakers multiplies its own

**Owner:** the kernel's log — `kernel/src/log/spoken.rs`, where the share is
charged, and `kernel/src/loader/start.rs`'s `build_child_handles`, where a
console is minted.

A program's console lines are records in the kernel's per-CPU shards, and one
console holder's share of them is bounded: `PROGRAM_BURST` (128) lines at once,
then `PROGRAM_PER_SEC` (16) a second (`kernel/src/log/spoken.rs`). The bound is
per holder, and a holder is minted at spawn: `build_child_handles` makes a new
`ConsoleObject` for every child that is handed one, and `ConsoleObject::new`
gives it a `Speaker` with a full `Share`. So the ring's bound is the rate times
the number of console holders, and any program that may spawn chooses that
number.

## Evidence

The arithmetic from the constants, not a measurement: a shard holds
`SHARD_RECORDS` (512) records (`kernel/src/log/shard.rs`), so four children
each spending their burst on one CPU fill that CPU's shard, and a fifth laps the
kernel's own records out of it before `logd` has read them. `console_flood_is_bounded`
measures one holder and nothing else; no test spawns speakers. `writeback_spawn`
already spawns a copy of itself off `/home`, so the mechanism is one any program
holding the spawn right has.

## Exit condition

A spawn cannot mint more of the ring than its spawner holds — the share is
charged to something a spawner cannot multiply — and a test in which one
program spawns speakers until its lineage has spent several bursts leaves the
kernel's records unlapped: `logd` reports no `record(s) were overwritten`, as
`console_flood_is_bounded` asserts for one holder.
