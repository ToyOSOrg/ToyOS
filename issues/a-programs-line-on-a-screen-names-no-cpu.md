---
status: open
kind: defect
opened: 2026-10-04
---

# A program's line on a screen names no CPU

The owner asked for the seconds since boot and the CPU in front of every
line a screen shows. A kernel record's line carries both: `[kernel 1.193 cpu0
alert tid=3]`. A program's line carries only the time:
`{1.234 warn soundserver}` on `/system/bin/console` and on `cargo run`'s
terminal. The reason is the record. `toyos::log::region::Body`, the record a
program writes into its ring, has `at_ns`, `pid`, `tid` and a severity and no
CPU, so `logkeeper` has no CPU to give `toyos_logstream::ProgramLine`.

Owner: orchestrator. Exit condition: one of two. Either a program's record
carries the CPU its writer stamped it on, every program line `logkeeper` writes
names it as `cpu<n>` after the time, and `toyos-logstream`'s
`a_screen_shows_the_consoles_line_from_either_form` requires it of a
program's line. Or the owner rules that a program's line needs no CPU, and
this file is deleted with that ruling in its commit message.
