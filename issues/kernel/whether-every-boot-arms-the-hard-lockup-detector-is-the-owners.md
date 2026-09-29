---
status: owner
kind: question
opened: 2026-09-29
---

# Whether every boot arms the hard-lockup detector is the owner's

`kernel/src/hardlockup` ends a machine one of whose CPUs has taken no interrupt,
with `IF` clear, for its bound: it seals a `WEDGED` record naming that CPU's
`pc` and `sp`, and resets the machine. It is armed only through
`boot-deadline=` (`deadline::start`, `kernel/src/deadline.rs:168-183`), so a
boot without that parameter, the owner's own machine included, has none, and
a CPU frozen there is a hand on the power button.

Arming it on every boot, at `toyos_tco::HARD_LOCKUP_BOUND_MS` where no
deadline names a bound, costs one NMI per busy CPU per second
(`SAMPLE_NS`, `kernel/src/hardlockup/mod.rs:75-80`; a halted CPU takes none),
and turns such a freeze on the owner's machine into a reset with a record. It
changes no ABI. No stage of
`issues/hardware/the-t14-reboots-through-ubuntu-for-every-test.md` waits on
it, since every session image carries `boot-deadline=`.

*Recommended: yes.*

**Exit**: the owner rules.
