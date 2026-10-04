---
status: open
kind: defect
opened: 2026-09-28
---

# The lock spin's shootdown poll says `IF` is clear, and two of its callers spin with it set

`Lock::lock` (`kernel/src/sync.rs`) polls TLB shootdowns inside its spin with the
comment "this spin runs with `IF` clear". Two callers spin there with `IF` set:

- a kernel thread, which `kernel_start` (`arch/x86_64/entry.rs`) enters with `sti`;
- the idle loop, whose `reap_finished` takes `PROCESS_TABLE.lock()`.

There the 0xFE IPI can land inside `tlb::poll`'s own `serve_if_owed`, so one
CPU runs a serve nested inside another. `Shootdown::serve` raises `flushed` with
`fetch_max` for exactly that case, and `kernel-loom`'s
`a_nested_serve_is_not_undone_by_the_one_it_interrupted` reds when it stores.
The comment gives the poll a reason that holds only for syscall context.

**Exit**: the comment states when the spin runs with `IF` set and when clear, and
why the poll is needed in each.
