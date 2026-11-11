---
status: open
kind: finding
opened: 2026-11-11
---

# The kernel stops every userland thread at a stop the supervisor orders

An open design question, the orchestrator's to resolve. Found by the
inventory in
`issues/the-kernel-is-small-interrupts-post-and-threads-wait.md`.

`SYS_SHUTDOWN` and `SYS_REBOOT` first stop every userland thread at its
next return to Ring 3 and wait for them inside a budget
(`kernel/src/quiesce.rs`, 216 lines, deciding by `toyos-quiesce`, 294, and
about 110 of `kernel/src/syscall/machine.rs`), then log the census and end
the machine. The supervisor owns the orderly stop and has already ended or
flushed what it started before it asks
(`issues/the-supervisor-is-host-tested-and-owns-the-stop.md`). The design
names the power-off as the kernel's and does not name this stop of
threads.

What is not established is what the kernel's stop holds that the
supervisor's does not: the census is of a machine that has stopped, and the
console wire is taken from its holder. Whether a supervisor that ends its
subtree before the call makes the kernel's sweep redundant is an inference
nobody has checked against the stop's tests.

**Exit**: what the kernel's sweep establishes that the supervisor's stop
cannot is written at `kernel/src/quiesce.rs`'s header and this file
deleted, or the sweep is removed as a stage of the track.
