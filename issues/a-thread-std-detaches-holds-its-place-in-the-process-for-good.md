---
status: open
kind: defect
opened: 2026-10-04
---

# A thread std detaches holds its place in the process for good

The kernel keeps an exited thread in its process's table until
`SYS_THREAD_JOIN` collects it, and counts it against
`toyos_abi::syscall::MAX_THREADS` until then
(`kernel::proclife::spawn::Admit::Full`). std's `Thread`
(`sdk/std/sys/thread.rs`) has no `Drop`, and a `JoinHandle`
dropped without `join` detaches, so nothing ever collects a thread std
detached: a program that detaches threads over its life has `thread::spawn`
refused once those that exited and those still running are
`MAX_THREADS - 1`, though not one of the exited ones runs. libc's
`pthread_detach` joins the thread itself once it has exited
(`userland/libc/src/pthread.rs`, `reap`); std has no such path.

Owner: orchestrator. Exit: a thread std detaches is collected once it exits,
and a test that spawns and drops more than `MAX_THREADS` handles, each thread
exiting, is refused none of them.
