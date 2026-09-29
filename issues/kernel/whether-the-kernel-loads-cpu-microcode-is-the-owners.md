---
status: owner
kind: question
opened: 2026-09-29
---

# Whether the kernel loads CPU microcode is the owner's

Microcode is firmware that runs on the CPU, which root `CLAUDE.md` does not
carve out: it admits only device firmware that never executes on the CPU. Until
the kernel loads it, a CPU whose BIOS ships stale microcode stays below Linux
at `Ubuntu-6.8.0-142.142` on every line that rests on microcode.

**Exit**: the owner rules.
