---
status: open
kind: defect
opened: 2026-09-27
---

# A `MANAGE` holder can kill a kernel thread

Every kernel thread (`kernel/src/sched/kthread.rs`) holds a process-table
entry and a pid. `SYS_PROCESS_OPEN` (`kernel/src/syscall/proc.rs`'s
`sys_process_open`) mints a `Process` handle for any pid a `SysCap` carrying
`Rights::MANAGE` names, a kernel thread's included, and `SYS_PROCESS_KILL`
then claims its teardown and posts its retire. Not run yet: a kernel thread has no
Ring 3 boundary to die at, so the expected end is the reaper's tripwire halting the machine
(`reaper: pid N not released after 10s`); killing the reaper itself leaves it
owing its own teardown to itself, with the same end.

Reachable only through init's capability today: init is the one `MANAGE`
holder. It is still userland crashing the kernel.

## Exit condition

A kernel thread's pid mints no `Process` handle — `sys_process_open` refuses it
as it refuses a pid that is gone — with a guest test holding `MANAGE` that
opens each kernel thread's pid from the boot's `kthread:` lines and is refused.
