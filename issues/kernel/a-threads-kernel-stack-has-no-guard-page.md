---
status: open
kind: defect
opened: 2026-09-29
---

# A thread's kernel stack has no guard page

A thread's 128 KiB kernel stack is a kernel-heap allocation in the direct map
(`kernel/src/loader/start.rs:26`), with live heap memory on either side. An
overflow writes into whatever the heap put below it; the only check is
`check_stack_canary`'s word at the stack's end, read on the thread's next
scheduler pass (`kernel/src/sched/driver.rs:521,592,925-935`), after the
write. The per-CPU idle stacks already have an unmapped page below them
(`kernel/src/arch/x86_64/percpu.rs:482`). Linux at `Ubuntu-6.8.0-142.142`,
under the T14's `CONFIG_VMAP_STACK=y`, maps every task stack in vmalloc space
between guard pages (`kernel/fork.c:314-318`, `arch/Kconfig:1327-1336`), so
the first write past the end faults. A row of the hardening table in
`issues/kernel/the-kernel-mitigates-what-linux-mitigates-on-the-t14.md`.

**Exit**: every thread's kernel stack has an unmapped page below it; a
`boot-actuators` arm that recurses a thread past its stack gets a double fault
naming that thread's guard page, and a stack allocated without the guard reds
it.
