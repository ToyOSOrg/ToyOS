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
`issues/no-program-measures-toyos-against-linux-on-one-machine.md`
close.

**Exit**: `wt/toyos-linux-sweep`, which holds this section and this slug
under #915's fence, settles the turbostat readings below.

## Linux's counter readings, read 2026-10-03

The orchestrator takes this Linux as the one independent oracle for the T14's
hardware counters (`issues/toyos-explains-itself.md`). Read as
root under Ubuntu's `6.8.0-142-generic` from 16:22:37 UTC, one after the
other, and committed whole with their commands in the `SOURCE` beside them:
turbostat idle and under one `yes` per CPU in `tests/metal/t14-linux/`, which
`counters_on_metal` prints ToyOS's reading beside.
Idle, per 10 s: busy 0.11 to 0.36%, busy clock 980 to 1668 MHz, TSC 2419
MHz, SMI 0. Loaded: busy 99.77%, busy clock 3800 MHz for the first two
intervals, 3555 MHz in the third, then 3075 to 3094 MHz; SMI 0.

Owner: the orchestrator, which holds the T14 the exit runs on.
