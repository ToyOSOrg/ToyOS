---
status: open
kind: track
opened: 2026-09-29
---

# Idle CPUs enter the C-states `_CST` names

The kernel runs no `mwait` and reads no `_CST`. The states come from `_CST`,
as Linux takes the T14's, after the AML interpreter
`issues/kernel/toyos-runs-the-machine-in-acpi-mode-and-interprets-its-aml.md`
records ToyOS lacks.

**Exit**: on the T14, since a KVM guest's idle is its host's: the idle
minute's package energy, and timer lateness no worse than `hlt`'s by more than
the chosen state's exit latency. **Mutation**: always the deepest state.
**Oracle**: Linux's figures.
