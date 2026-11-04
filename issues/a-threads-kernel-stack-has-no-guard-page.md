---
status: open
kind: defect
opened: 2026-09-29
---

# A thread's kernel stack has no guard page

A thread's 128 KiB kernel stack is a kernel-heap allocation in the direct map
(`KernelStack`, `kernel/src/loader/start.rs`), with live heap memory on either side. An
overflow writes into whatever the heap put below it; the only check is
`check_stack_canary`'s word at the stack's end, read on the thread's next
scheduler pass (`kernel/src/sched/driver.rs`), after the
write. The per-CPU idle stacks already have an unmapped page below them
(`kernel/src/arch/x86_64/percpu.rs`). Linux at `Ubuntu-6.8.0-142.142`,
under the T14's `CONFIG_VMAP_STACK=y`, maps every task stack in vmalloc space
between guard pages (`kernel/fork.c:314-318`, `arch/Kconfig:1327-1336`), so
the first write past the end faults.

**Exit**: every thread's kernel stack has an unmapped page below it; a
`boot-actuators` arm that recurses a thread past its stack gets a double fault
naming that thread's guard page, and a stack allocated without the guard reds
it.

Every kernel stack is now a user thread's or a CPU's idle loop's (#1165
deleted the kernel threads). What the design has to meet: AArch64's vectors run on the interrupted `SP_EL1`, so a guard
is reached only once an EL1 entry checks `sp` against the running stack's
bound and reports from a per-CPU overflow stack, which the idle stacks' guard
lacks today too; and a stack slot's guard hole in the direct map decides when
its memory can go back to the PMM.
