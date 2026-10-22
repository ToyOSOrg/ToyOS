---
status: open
kind: defect
opened: 2026-10-22
---

# An ended detached thread is reaped only at its process's next spawn

The kernel releases a thread only when another thread joins it
(`SYS_THREAD_JOIN`). Nothing can be told that no thread ever will. A thread std
detaches can't free the stack it runs on, so as it ends it pushes its block
onto `REAPED` (`sdk/std/sys/thread.rs`). The next `Thread::new` in the same
process joins it, which frees its 2 MiB stack and its `MAX_THREADS` place.

Until that spawn, the ended thread stays in its process's table. `ps` shows it
as `Z`, and its stack stays allocated. A process that detaches threads and then
never spawns another keeps all of them for its whole life: any program that
drops a `JoinHandle` and starts no thread after it. The cost is bounded by the threads detached since
the last spawn, not by the number of spawns. #987 made it bounded; before #987
nothing reaped these threads at all.

Measured on QEMU's x86-64 machine booting `tests/images/testcases.toml`, with
toybox `ps` after the boot:

- At `ff9b4b1c5`, which has neither #987 nor a join, the supervisor's two
  `bootstrap` reader threads, which the supervisor had detached, show `Z` for
  the rest of the boot.
- At `25d787482`, which has #987 but not that commit's join (a scratch revert),
  no `bootstrap` thread is listed. The supervisor's later thread spawns had
  reaped them.

The case this record is about, a process that never spawns again, was not
measured. It follows from `Thread::new` being the only reaper.

*Exit:* the kernel releases an ended thread that nobody will join. That means
a detach call, or a flag at thread creation, so the thread's place goes when
it ends. std's detach then frees the stack at once: by the kernel unmapping it
as part of the thread's end, or by std freeing it from a thread that is still
alive. `REAPED` is deleted. libc's `pthread_detach` (`sdk/libc/src/pthread.rs`)
uses the same pattern and moves with it.

Owner: the orchestrator.
