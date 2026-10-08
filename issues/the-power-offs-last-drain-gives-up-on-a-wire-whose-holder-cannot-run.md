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
`PANIC_LOCK_SPIN_LIMIT` times (`serial.rs:131`, `:264`, `serial_lock::within`)
and, if the wire never came free, appends one line to the black box and
returns: the machine powers off with its last records undrained.

The wire is a `SleepLock` "held with interrupts on and preemption allowed"
(`serial.rs:278-279`), and `klogd` takes it once per pass
(`kernel/src/log/console.rs:417`, `body`: `serial::wire(&parkable)`). On the
way to the power-off, `quiesce` (`kernel/src/syscall/machine.rs:69`) logs the
boot's last word (`:109`) and calls `console::drain_inline` (`:112`), which
declines a held wire (`console.rs:94`:
`let Some(wire) = serial::try_wire() else { return }`). Its last call is
`xhci::seal_shut` (`machine.rs:132`), which, where it gets the controller
lock, forgets the guard: "Preemption stays disabled with it"
(`kernel/src/drivers/xhci/mod.rs:1550`; the `forget` is `:1565`).

The seal is the point this turns on. Up to it nothing read here takes
preemption from the stop's thread (it parks at `:94`), so a `klogd` holding
the wire when `drain_inline` declined can still run, finish its pass and let
go: nothing between `:112` and `:132` prevents that. From a seal that was
taken to the power-off the stop's thread has preemption off. So on one CPU,
a `klogd` descheduled inside its hold when `seal_shut` takes the controller
lock is never run again: `flush_final`'s count is then a wait on a
holder that cannot run, and it expires. Whatever that hold had not yet put on
the wire, and every record committed after it, is on no console. On more than
one CPU the holder runs elsewhere and lets go. Where the seal is refused
(`seal_shut`'s `None` arm) no guard is forgotten, and this reading does not
apply.

**Not established**: how `klogd` comes to be descheduled inside its hold at
the seal. `drain_for_the_stop` (`machine.rs:94`) parks on the wire and so
finds it free once, but the two censuses, the stop's record and the last
word are logged after it (`:96`-`:109`) and before the seal, and a record's
commit wakes `klogd`. No run shows the loss. `machine_shutdown_short_stop`
waits on `Shutting down.` on one CPU through this drain, so its history on
`main` is the base rate.

## Owner

The orchestrator.

## Exit condition

`flush_final` waits on the wire's release where its holder can still run, or
the stop keeps the holder from being left inside its hold at the seal; and a
one-CPU guest test, whose actuator leaves `klogd` descheduled inside its hold
of the wire as the stop's thread takes the seal in `seal_shut`, reads
`Shutting down.` on the console. That test is red on a kernel whose last
drain is today's.
