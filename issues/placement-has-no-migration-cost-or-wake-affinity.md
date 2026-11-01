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
  (parallelism). The first half is in `CpuSched::toward_its_waker`:
  - Every wake a thread posts names its CPU; one a device interrupt's
    handler, a deadline or a retire posts names none.
  - The wake that makes `WAKER_RUN` (8) in a row from one other LLC moves
    the task to a sleeping CPU there that its mask names and no kick is on
    its way to, so a pair split across LLCs shares one after 8 round trips.
    A split pair costs about 25% on the AMD bench's `ipc.reqrep.8` and
    `fs.disk.read_4k_at`.
  - Only a wake that finds the task parked is counted. A server with a
    client in each LLC therefore follows the far one when the near one's
    requests keep finding it running: 5 of 40 runs of the simulator's
    `server_of_two_llcs` move it once, whatever the run length from 8 to
    12, and none moves it back at ten times the rounds; a run of 4 does.
    Parked wakes from both LLCs, independent and even, move it on about one
    in 510. No metal row has a server with clients in both LLCs.
  - The task's home CPU makes the move after its own wake, so a move costs
    two wakes in series.
  - Spreading a waker's partners for parallelism is still undecided.
  - **Owner ruling, 2026-11-01:** one u16 of per-task placement state is
    admitted, kept by the CPU that owns the task: the LLC of its last waker,
    which holds the run's count beside it. It is the single exception to the
    2026-10-28 scheduling model's "no per-task state".
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
