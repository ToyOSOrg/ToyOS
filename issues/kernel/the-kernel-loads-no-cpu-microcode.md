---
status: open
kind: defect
opened: 2026-09-29
---

# The kernel loads no CPU microcode

The kernel is to load CPU microcode signed by the CPU's maker, pinned by
version and hash the way vendor device firmware is (owner, 2026-09-30). It
loads none, so a CPU whose BIOS ships stale microcode stays below Linux at
`Ubuntu-6.8.0-142.142` on every line that rests on microcode.

Root `CLAUDE.md`'s firmware rule admits this microcode once PR #636 lands.

**Exit**: the kernel loads current microcode early on every CPU, at least as
current as Linux's.
