---
status: open
kind: tooling
opened: 2026-09-24
---

# The deaf-CPU actuator arms on three seconds of guest clock

`kernel/src/sched/dump.rs`'s `deaf_window` (`dump-deaf-cpu`, driven by the
nightly `dump_nmi_probe`) does nothing until `nanos_since_boot()` reaches its
`ARM_AT_NS`, 3 s, "late enough that the machine is up and every CPU has
joined". That is a sleep standing in for an event: a boot slower than 3 s puts
the deafening inside it, and a fast one idles until the clock passes. The
harness (`tests/common/faults.rs`'s `dump_nmi_probe`) prices its 20 s drain on
the same number.

The same shape in `dump-in-blocking-pass` was a review BLOCKER on the branch
that filed this and was replaced by arming on `smp::is_ready()`, the SMP
release that is the machine's own word that every CPU has joined. This one was
off that branch's path.

**Exit:** `deaf_window` arms on an event, the harness reads the boot log for
whatever it prints first, and `dump_nmi_probe` passes on a boot slowed past
3 s.
