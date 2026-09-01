---
status: open
kind: track
opened: 2026-09-01
---

# Nothing can freeze the whole machine from outside, so the last case the blocked dump exists for is unreachable

`issues/diagnostics/blocked-dump-cannot-fire-on-a-total-freeze.md` records that
the request and service path runs only through `drain_irqs`
(`kernel/src/sched/driver.rs:652`), so every trigger the tree owns presupposes a
CPU that can still schedule that path.

**Most of the machinery is already built, and this track is only the last piece.**
`tests/toyos.rs:6628`'s `freeze_report` opens a QMP monitor, takes
`info registers -a` *before* injecting anything, injects the dump trigger over
QMP as key events, then waits on serial for `=== end of dump ===` under a
30-second host deadline. That is register capture, host-side injection, serial
capture and a deadline — four of the five things the instrument needs.
`kernel/src/sched/dump.rs:240`'s `deaf_window` is an actuator for a CPU that
does not answer, and `tests/common/faults.rs:760`'s `dump_nmi_probe` asserts the
returned `rip` lands inside it (`:820`). So the partial case — one CPU deaf,
others scheduling — is reached today.

**What is missing is the whole-machine case.** No QMP `stop`/`cont` appears
anywhere in `tests/` or `src/`: nothing suspends every vCPU and then asks
whether the dump path can still deliver. That is the state the dump exists for
and the one no guest workload can certify, because a guest that can arrange the
freeze can no longer report on it.

**Two arms, because one of them can lie.** QMP `stop` can itself prevent the
handler from running, which looks identical to a dump path that does not work.
So the actuator is accepted only when both arms run — a "busy but interruptible"
guest and a QMP-stopped one — with the register trace distinguishing them. An
actuator that only ever produced the second is a tautology, not an instrument.

**It joins the existing socket.** `BootOptions::qmp` is opt-in and per-instance,
and the monitor serves one caller at a time; the actuator uses that socket
rather than opening a second.
