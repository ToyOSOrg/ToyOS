---
status: open
kind: finding
opened: 2026-11-05
---

# k dying corpses take k deferral grants on one CPU

A dying task on a CPU defers real-time work for at most one
`DYING_CHUNK_NS` grant once it has aged `DYING_AGE_NS`
(`kernel/pure/sched/cpu.rs`, `CpuSched::dying`). The bound is per corpse and
not per CPU: k corpses on one CPU take k consecutive grants, and a real-time
band that briefly empties throws the accumulated age away on every dispatch.

The planned exit was a per-CPU dying server holding a reservation, which the
owner's 2026-11-05 rejection of reservations
(`issues/cpu-time-is-a-band-and-not-a-reservation.md`) removed. No measurement
shows the effect on audio; the per-CPU real-time allowance of owner decision J2
is the bound a replacement would work within.
