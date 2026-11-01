---
status: open
kind: track
opened: 2026-08-16
---

# Placement has no migration cost or wake affinity

Queued track from the 2026-08-16 owner conversation on scheduler direction.

Placement reads the machine's cache tree (`kernel/pure/sched/topology.rs`):
a wake forward, a push and a steal probe look nearest first, a forward or a
push takes a free physical core before an SMT thread of a busy one inside each
cache domain, and a spawn prefers a sleeping CPU on a free core. What it still
lacks:

- **Last-CPU bias with a migration-cost model**: prefer the previous core unless
  the imbalance pays for the cache refill. A move across an LLC costs the same
  to the placement code as one inside it.
- **Wake-affine vs spread**: on wake, decide between placing a task near its
  waker (shared data stays warm — pipe/IPC pairs) and away from it
  (parallelism). Branch `sched-wake-llc` does the first half.
  - Every wake a thread posts names its CPU.
  - A task woken twice in a row from one other LLC moves to a sleeping CPU of
    that LLC, so a pair split across LLCs shares one.
  - A server woken from two LLCs in turn stays put.
  - Measured on the AMD bench (ab3): a split pair costs about 25% on
    `ipc.reqrep.8` and `fs.disk.read_4k_at`.
  - Spreading a waker's partners for parallelism is still undecided.
  - **Owner ruling, 2026-11-01:** the one u16 of per-task placement state is
    admitted: the LLC of the task's last waker, kept by the CPU that owns the
    task. It is the single exception to the 2026-10-28 scheduling model's "no
    per-task state".
- **An idle-state input**: see `issues/the-scheduler-and-the-clocks-never-talk.md`.
- **Bounded scans.** Each choice walks the rings it needs whole. A steal
  probe reads the machine's offering mask first, so a machine with nothing to
  steal costs an idle pass one line; once a CPU offers surplus, the probe and
  every wake forward still read every handle in their rings, a cost that grows
  with the CPU count. Derived from the code, not measured; both benches have
  at most 16 CPUs.

Composes with the reservation model (placement decides *where*, reservations
decide *how much*) and with the scheduler simulator (`kernel/sim`), which runs
every scenario at the benches' scouted shapes and two permuted synthetic trees
(`kernel/sim/src/topology.rs`).
