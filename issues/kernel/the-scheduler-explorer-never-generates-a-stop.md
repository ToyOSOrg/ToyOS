---
status: open
kind: tooling
opened: 2026-09-25
---

# The scheduler's interleaving explorer never generates a stop

`toyos-sched/sim` explores interleavings of blocks, wakes, migrations and
teardowns across CPUs, but no `Op` it generates marks a task `STOP`. The machine's
stop adds three routes into the per-CPU `stopped` band:

- `TaskShared::stop_if_blocked`'s CAS against a parked task;
- `SchedPass::dispose_stop` at a running task's Ring 3 boundary;
- `CpuSched::place` banding a woken or adopted task that already carries the
  mark.

Each of them races a cross-CPU wake, an adopt in transit or a retire. The only
coverage is the single-CPU unit tests in `toyos-sched/src/cpu.rs` and
`toyos-sched/src/task.rs`, which drive each route once and in order. The sim's
invariants include I1, every task in exactly one container. None of them has
been checked with a stopped task in the machine.

**Exit condition**: the explorer generates `stop_if_blocked` and a stop at a
running task's safe point, interleaved with its existing `Block`, `Wake` and
`Teardown` ops, and I1 holds across every schedule it runs.
