---
status: open
kind: track
opened: 2026-11-12
---

# A claimed wake ignores a queued task of its band at home

The approved placement policy's rule 2 sends a wake to an idle CPU when its
home runs a task of the wake's band, *or publishes a queued task of that
band*. #1274 implements the first half only. A claimant reads what would
queue behind the home's running task (`CpuHandle::behind`,
`kernel/pure/sched/cpu.rs`), which is the predicate the home's own `place`
reads, and neither weighs the home's queue.

So a fair wake whose home runs an RT task, with fair work already queued
there, goes home as a `Wake`. It then queues behind that fair work while an
idle CPU may sit beside it, until the home's pick reaches it or a steal
moves it. Under the clause, its claimant would have sent it to the idle CPU
at once.

**Exit**: the owed A/B of #1274
(`issues/a-wake-onto-a-busy-cpu-costs-toyos-1-8-times-linuxs.md`) measures a
wake whose home runs a task of another band with one of the wake's band
queued. No `osbench` row stages that yet, so the A/B owes one. If that wake
waits longer than one sent straight to an idle CPU, implement the clause.
The home publishes its queued bands beside `behind`, the claimant reads
them, and the home's `place` weighs its queue as `place_settled` does.
Otherwise, close this as not needed.

**Owner**: the scheduler's wake path (`kernel/pure/sched/`).
