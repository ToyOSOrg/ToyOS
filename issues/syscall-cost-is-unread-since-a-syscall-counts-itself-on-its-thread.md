---
status: open
kind: defect
opened: 2026-10-12
---

# `syscall_cost` is unread since a syscall counts itself on its thread

`syscall_dispatch` (`kernel/src/syscall/dispatch.rs`) took `PROCESS_TABLE`'s
lock twice per syscall, cloned the thread's `Arc` and took its `ThreadData`
lock, to update counters that belong to one thread. Since #1061 a syscall
counts itself on its thread's `TaskHandle` (`SyscallProfile`, written by that
thread with a load and a store) and takes no machine-wide lock and no `Arc`.

Read on the T14 in #1061's A/B (one osbench boot per side): `syscall.null`
fell to 0.42× of `main`'s. The AMD bench's reading and the `syscall_cost`
row on both benches are still owed.

**Exit**: `syscall_cost` read on both benches before and after, posted.
