---
status: open
kind: tooling
opened: 2026-10-12
---

# `syscall_cost` is unread since a syscall counts itself on its thread

A syscall counts itself on its thread's `TaskHandle` (`SyscallProfile`,
`kernel/src/sched/payload.rs`) and takes no machine-wide lock to do it.

Read on the T14 in #1061's A/B (two rounds, two boots a side in the second): `syscall.null`
fell to 0.42× of `main`'s. The AMD bench's reading and the `syscall_cost`
row on both benches are still owed.

**Exit**: `syscall_cost` read on both benches before and after, posted.
