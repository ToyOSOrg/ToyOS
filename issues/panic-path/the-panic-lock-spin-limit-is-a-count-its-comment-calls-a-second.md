---
status: open
kind: defect
opened: 2026-09-27
---

# The panic lock spin limit is a count its comment calls a second

`PANIC_LOCK_SPIN_LIMIT` in `kernel/src/drivers/serial.rs` bounds two waits:
`panic_flush`'s for `BackendGuard`, and `flush_final`'s for the console wire.
It is 100,000,000 iterations of a `try_lock` and a `pause`, and its comment
calls that "~1s of spin". Nothing converts it to time, so the wait lasts
whatever that many `pause`s cost on the CPU running them, and nobody has
measured that on any machine this kernel boots.

It stopped a machine from resetting once. In the orchestrator's
`cargo test --test toyos-build -- --nightly syscall_panic_halts` at `f9700b90`,
EXIT=1, the guest printed `returning this machine to firmware` and QEMU reported
no reset for the rest of the harness's wait. The log prints a 2.96x liveness
width, so that wait was 25 s x 2.96 plus the 20 s drain after it, about 94 s.
The serial shows `klogd` stopped after one 16-byte burst of a line
(`[kernel 0.573 cp`). The halt IPI is a fixed vector, so a CPU stops only with
interrupts on, and the one console lock held with interrupts on is the wire.
The panic path's reset then went through `acpi::reboot`, whose `flush_final`
spun this count on that wire. The panic path now resets through
`acpi::reset_now` and does not wait on the wire. Both waits above still take
the count.

**Evidence:** the run above, and the code.

**Exit condition:** both waits are bounded by time against the counter the
panic path already times its bound with, and the comment says what is measured.
