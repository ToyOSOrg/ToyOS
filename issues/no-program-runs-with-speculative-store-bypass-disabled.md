---
status: open
kind: defect
opened: 2026-09-29
---

# No program runs with speculative store bypass disabled

The kernel never sets SSBD, so no process can have it. Linux at
`Ubuntu-6.8.0-142.142` defaults to `prctl`: a task that asks runs with it
(`arch/x86/kernel/cpu/bugs.c:2172,2278`). In ToyOS a program asks by a flag in
`system.toml`, and a flagged program's children inherit it at spawn.

**Exit**: on every proving machine a flagged program runs with SSBD set,
through the control
`issues/a-pure-function-decides-a-cpus-speculation-mitigations-as-linux-does.md`
names for its CPU, and so does its child, while an unflagged one runs with it
clear. **Mutation**: no inheritance. **Oracle**: the CPU's own register.
