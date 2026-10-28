---
status: assigned
kind: finding
opened: 2026-10-12
---

# A wake whose waker parks next is forwarded to an idle CPU

`CpuSched::place` (`kernel/pure/sched/cpu.rs`) hands a fair wake that would
queue behind the running fair task to an idle sibling. When the waker is about
to park on that same CPU — a request answered and waited on, a thread started
and then joined — the CPU is free a moment later, and the forward pays an IPI
and a cold cache where a local switch after the park was free. Nothing tells
`place` that the running task is about to park.

`acpi_mediated` met it on a guest: a thread started from a busy CPU went to the
idle one, so it stages its asker differently since PR #874. On metal the
spawn-then-wait row reads no change past one boot's spread: `spawn_cost`'s
median went from 1,025 to 1,029 µs on the T14 and from 2,054 to 2,086 µs on
the AMD bench, base to head, one boot each, read from the second judgement's
`testcases` readbacks.

Owned by stage S3 of the scheduler design (#872), whose 1:1 pair rule decides
where a pair's wakes go.

Branch `sched-place` decides it: the opening drain of a pass holds a wake
found behind the running task until the disposition, so a poster that parks
or exits leaves it to run here, and one that yields or keeps the CPU still
forwards it (`CpuSched::place`; the simulator's `turn_taking_pair`). The
metal price is what remains.

**Exit**: S3 decides this case, with a simulator scenario of a wake whose waker
parks next and a metal row that prices the forward against the local switch.
