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
  (parallelism). Every wake still ignores the waker.
  PR #1129 built the near half and closed on its AMD A/B, ab10
  ([judgment](https://github.com/ToyOSOrg/evidence/blob/d1dbb3c0a9511862295dab4b7cdcd1d24f47365a/pr-1129/r9/judgment.md)).
  - **Its design:** each wake names its poster's CPU. A wake from a device interrupt, a deadline, a retire, or a thread
    that has begun leaving its process names none. A task whose last 8 parked wakes came from one other LLC moves to a
    sleeping free core there, off the waker's own core.
  - **What it bought:** `ipc.reqrep.8` −14% (−40% at 65536), the file server's metadata rows −14 to −19%, and
    `work.build_shaped.j16` −2%. The build's parent is woken by the file server, not by its children.
  - **What it cost:** about +5% on `mem.munmap_touched_remote.4k` in every boot with the plumbing (ab5–ab10), with or without
    the moves. The mechanism is not found.
  - **Constraints a successor must keep:**
    - A run shorter than 8 makes a server with a client in each LLC bounce (simulator, 4 in one run).
    - A partner moved onto its waker's SMT sibling reads as slow as split.
    - Leaving threads must name no waker.
  - **Measuring it:** where a split pipe pair lands decides `pipe.throughput`. Several rows are bimodal per boot on this bench,
    so an A/B there needs per-row mode accounting.
- **An idle-state input**: see `issues/the-scheduler-and-the-clocks-never-talk.md`.
- **Bounded scans.** Each choice walks the rings it needs whole. A steal
  probe reads the machine's offering mask first, so a machine with nothing to
  steal costs an idle pass one line; once a CPU offers surplus, the probe and
  every wake forward still read every handle in their rings, a cost that grows
  with the CPU count. Derived from the code, not measured; both benches have
  at most 16 CPUs.

Composes with the scheduler simulator (`kernel/sim`), which runs
every scenario at the benches' scouted shapes and two permuted synthetic trees
(`kernel/sim/src/topology.rs`).
