---
status: open
kind: defect
opened: 2026-09-27
---

# std and libc drop the answer `thread_join` gives

`sdk/std/sys/thread.rs`'s `Thread::join` and
`userland/libc/src/pthread.rs`'s `pthread_join` call
`toyos_abi::syscall::thread_join` and discard what it returns. A join the
kernel refuses — `NotFound` for a tid it never had or already collected,
`Gone` for a cancelled wait — reads as a thread that finished. std's unix
`join` panics on the same refusal, and `pthread_join` returns 0 for it.

It hid a kernel defect: from #513 until the fix beside this file,
`sys_thread_join` answered `NotFound` to every joiner that parked before its
thread exited, and only `fpu_isolation`, which asserts the answer, saw it.

**Exit**: std's `join` fails loudly on a refused join and `pthread_join`
returns the error, each with a guest test that joins a tid twice.
