---
status: owner
kind: question
opened: 2026-09-29
---

# Whether every boot arms the hard-lockup detector is the owner's

What it is: a performance counter on each CPU raises an NMI about once a second
of that CPU's busy time, and the NMI checks whether the CPU has taken any other
interrupt since the last one. A CPU that has taken none for
`toyos_tco::HARD_LOCKUP_BOUND_MS` with interrupts masked is frozen: it will
never run a thread again, and nothing else on the machine says so. The detector
(`kernel/src/hardlockup`) then seals a `WEDGED` record naming that CPU, its `pc`
and `sp` and the lock it spins on, and resets the machine. Linux runs the same
detector on every boot by default (`nmi_watchdog`,
`Documentation/admin-guide/lockup-watchdogs.rst`) and panics on it only where
`hardlockup_panic` is set; Windows ends such a machine with bug check 0x101,
`CLOCK_WATCHDOG_TIMEOUT`.

It is armed only through `boot-deadline=` (`deadline::start`,
`kernel/src/deadline.rs:168-183`), so a boot without that parameter, the
owner's own machine included, has none, and a CPU frozen there is a hand on the
power button. Arming it on every boot, at `HARD_LOCKUP_BOUND_MS` where no
deadline names a bound, costs one NMI per busy CPU per second (`SAMPLE_NS`,
`kernel/src/hardlockup/mod.rs:75-80`; a halted CPU takes none), and turns such
a freeze into a reset with a record. It changes no ABI.

A session image of
`issues/hardware/the-t14-reboots-through-ubuntu-for-every-test.md` names no
`boot-deadline=`, so without this it has no detector. No stage waits on it.

*Recommended: yes.*

**Exit**: the owner rules.
