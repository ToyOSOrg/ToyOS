---
status: open
kind: track
opened: 2026-08-16
---

# Fairness is per-thread, so thread count wins

Queued track from the 2026-08-16 owner conversation on scheduler direction.

**The premise this was opened on has been answered one level up; the remaining
work is the rest of the hierarchy.** Fair scheduling no longer shares the CPU
between *threads*. All threads of one process share a vruntime, so a second
runnable thread bumps a refcount and buys no second slice
(`toyos-sched/src/fair.rs:95-99`), and the state it advances is one `KShare`
per `Pid` (`kernel/src/scheduler.rs:249`, minted at `:257-268`). A process with
100 threads no longer out-schedules one with 8 by showing up more.

What is unbuilt is everything *above* the process: shares are one flat level
per `Pid` with no grouping over them. Production schedulers make shares
hierarchical: groups first, processes and their threads within their group.

ToyOS has a better grouping handle than Unix's cgroups bolt-on ever was: the
capability domain. A process's share can hang off what its parent endowed it
with, in the same `system.toml` that endows everything else — hierarchical
reservations, since reservations nest naturally. The reservation model is the
substrate; this issue is the hierarchy on top of it.

Blocked on: the reservations implementation. Design question to settle at pick-up:
whether the hierarchy's nodes are processes, endowment parents, or named domains
in `system.toml`, and what the default share of an unconfigured process is.
