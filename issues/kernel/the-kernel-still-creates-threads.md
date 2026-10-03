---
status: open
kind: track
opened: 2026-09-27
---

# The kernel still creates threads

The owner's ruling: the kernel creates no thread but the per-CPU idle loop.
Kernel work runs, bounded, on the thread or interrupt that caused it and is
charged to it; long-running work with no owner is a userland server's. The
ruling holds only if the whole kernel-thread machinery is deleted —
`sched/kthread.rs`, `kthread::spawn`, the row table and every policy a row
carries, `is_kernel_task`/`current_is_kernel_thread`, and every special case that exists
only for kernel threads. Stopping new uses is not enough.

**Exit condition:** `kernel/src/sched/kthread.rs` does not exist, and no kernel
code creates a schedulable task other than the per-CPU idle loop.

**Evidence:** `git grep -n 'kthread::spawn(' -- kernel/src`.

**Stages:**

- **K4:** `klogd` goes: the console moves to logd, and this track owns that move. The boot before logd runs
  and the panic path write the console wire (`log::console::drain_inline`,
  `serial::panic_flush`), so one wire's driver stays in the kernel whatever
  K4 moves.
- **K6:** delete the machinery named above. Blocked on K4.
