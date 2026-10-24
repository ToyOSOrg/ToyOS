---
status: open
kind: tooling
opened: 2026-09-29
---

# Linux's readings of the T14 and the TCG model lack reads owed before the T14's wipe

The CPU readings this issue owed went with `toyos-cpuvuln` (#915); the
track's pin is in
`issues/the-kernel-is-at-least-as-secure-as-linux-on-every-machine-toyos-supports.md`.
Ubuntu leaves the T14 only after this issue and
`issues/no-published-run-compares-toyos-with-linux-on-one-machine.md`
close.

**Exit**: the Linux readings on the T14 that
`issues/toyos-explains-itself.md` (each counter ToyOS reads there) and
`issues/no-published-run-compares-toyos-with-linux-on-one-machine.md` (that
program's outputs under Ubuntu) compare against are taken before the wipe and
published in evidence.

## Linux's counter readings, read 2026-10-03

A measurement of Linux on the same machine, never a test's oracle. Read as
root under Ubuntu's `6.8.0-142-generic` from 16:22:37 UTC, one after the
other, and published whole with their commands in
[evidence](https://github.com/ToyOSOrg/evidence/tree/423044d281a0e0df5bde10688b2baf59ca2be9b6/pr-929/t14-linux):
turbostat idle and under one `yes` per CPU.
Idle, per 10 s: busy 0.11 to 0.36%, busy clock 980 to 1668 MHz, TSC 2419
MHz, SMI 0. Loaded: busy 99.77%, busy clock 3800 MHz for the first two
intervals, 3555 MHz in the third, then 3075 to 3094 MHz; SMI 0.

Owner: the orchestrator, which holds the T14 the exit runs on.
