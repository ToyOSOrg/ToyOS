---
status: open
kind: defect
opened: 2026-10-08
---

# The power-off's last drain counts iterations on a wire whose holder may be unable to run, and then powers off without the tail

Read from the code at `d78350c71`, not run. Named by the review of pull
request #767 (its comment 6058801505); every line below was read again here.

`serial::flush_final` (`kernel/src/drivers/serial.rs:263`) is the last drain
before `power::shutdown` and `power::reboot` end the machine
(`kernel/src/power.rs:21`, `:53`). It asks `try_wire` up to
`PANIC_LOCK_SPIN_LIMIT` times (`serial.rs:131`, `serial_lock::within`) and, if
the wire never came free, appends one line to the black box and returns: the
machine powers off with its last records undrained.

The wire is a `SleepLock` its holder keeps "with interrupts on and preemption
allowed" (`serial.rs:277`), and `klogd` takes it once per pass
(`kernel/src/log/console.rs`, `body`: `serial::wire(&parkable)`). On the way
to the power-off, `quiesce` (`kernel/src/syscall/machine.rs`) logs the boot's
last word and calls `console::drain_inline` (`:112`), which declines a held
wire (`console.rs:90`: `let Some(wire) = serial::try_wire() else { return }`),
and then `xhci::seal_shut` (`:132`), which, where it gets the controller lock,
forgets the guard: "Preemption stays disabled with it"
(`kernel/src/drivers/xhci/mod.rs:1561`).

So on one CPU, a `klogd` descheduled inside its hold when the stop's thread
passes `drain_inline` is never run again: the stop's thread has preemption off
from the seal to the power-off, `flush_final`'s count is a wait on a holder
that cannot run, and it expires. `Shutting down.` and every line logged after
it are then on no console. On more than one CPU the holder runs elsewhere and
lets go.

**Not established**: how `klogd` comes to be descheduled inside its hold at
that point. `drain_for_the_stop` (`machine.rs:95`) parks on the wire and so
finds it free once, but the two censuses, the stop's record and the last
word are logged after it and before `drain_inline`, and a record's commit
wakes `klogd`. No run shows the loss. `machine_shutdown_short_stop`
waits on `Shutting down.` on one CPU through this drain, so its history on
`main` is the base rate.

## Owner

The orchestrator.

## Exit condition

`flush_final` waits on the wire's release where its holder can still run, or
the stop keeps the holder from being left inside its hold; and a one-CPU guest
test, whose actuator leaves `klogd` descheduled inside its hold of the wire as
the stop's thread passes `drain_inline`, reads `Shutting down.` on the console.
That test is red on a kernel whose last drain is today's.
