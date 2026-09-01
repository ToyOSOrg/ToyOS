---
status: open
kind: track
opened: 2026-09-01
---

# Nothing can freeze this machine from outside, so the blocked dump cannot be tested where it matters

`issues/diagnostics/blocked-dump-cannot-fire-on-a-total-freeze.md` records that
the request and service path runs only through `drain_irqs`
(`kernel/src/sched/driver.rs`), so every trigger the tree owns presupposes a CPU
that can still schedule that path. The state the dump exists for is exactly the
state in which no such CPU exists — and no guest workload can certify it,
because a guest that can arrange the freeze can no longer report on it.

**What to build.** A host-side actuator over QMP: freeze schedulable progress,
inject the architecture's NMI or dump signal, capture registers and the serial
and panel output, and enforce a host deadline so a failure to deliver is a
verdict rather than a hang. The evidence is the register trace — it proves
delivery independently of anything the guest chose to print.

**Two arms, because one of them can lie.** `stop` over QMP can itself prevent
the handler from ever running, which would look identical to a dump path that
does not work. So the actuator is accepted only when both arms are run: a
"busy but interruptible" guest and a QMP-stopped one, with the register trace
distinguishing them. An actuator that only ever produced the second is not an
instrument, it is a tautology.

**Reuse.** Total-freeze diagnostics and the panic fallback path both need this;
neither has any way to reach its own terminal state today.

**Cost is real.** This is new host machinery against the QMP surface, and the
harness already owns one QMP socket per guest — the actuator joins that, it does
not open a second.
