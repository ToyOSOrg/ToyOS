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

- **K2:** the reaper PR #549 introduces becomes last-thread-out: the victim's
  last thread tears down its own process on its way out of the kernel, and the
  scheduler frees that thread's kernel stack after switching away. Blocked on
  #549 landing.
- **K3:** the test-only `logstorm`/`lognest` producers are deleted if the log
  gate does not need kernel-context producers. Blocked on nothing.
- **K4:** `klogd` goes: the owner-approved driver-model design moves the
  console to logd, and this track owns that move.
- **K6:** delete the machinery named above. Blocked on K2–K4.
