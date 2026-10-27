---
status: open
kind: track
opened: 2026-09-29
---

# Idle CPUs enter the C-states `_CST` names

The kernel runs no `mwait` and reads no `_CST`. The states come from `_CST`,
as Linux takes the T14's, after the AML interpreter
`issues/toyos-runs-the-machine-in-acpi-mode-and-interprets-its-aml.md`
records ToyOS lacks. What `hlt` alone costs today, firmware's C1E promotion
included, is `issues/a-cpu-leaving-hlt-runs-its-first-microseconds-slow-under-c1e.md`.
On the T14 `hlt` also holds a one-thread load to the turbo ratio for three or
more active cores, 38 against 42
(`issues/toyos-uses-what-modern-hardware-offers-for-speed.md`). PR #1066's
scout, every idle wait `mwait 0x31` (the T14's `_CST` C2), lifted
`cpu.clock` to 4189 MHz in 13 of 17 osbench groups and ran `crunch.single`
1.10 to 1.11 times faster. It also ran `work.build_shaped.j1` 1.22 times
slower, against a control on a different `main`. Whether that is the
always-deep state's exit on every wake is unread.

**Exit**: on the T14, since a KVM guest's idle is its host's: the idle
minute's package energy, timer lateness no worse than `hlt`'s by more than
the chosen state's exit latency, and osbench's `cpu.clock` at ratio 42.
**Mutation**: always the deepest state. **Oracle**: Linux's figures.
