---
status: open
kind: defect
opened: 2026-09-28
---

# libc's pthread keys and `pthread_self` answer for the process, not the thread

`userland/libc/src/pthread.rs` indexes `pthread_getspecific` and
`pthread_setspecific` by `thread_index()`, which is `getpid() % 64`, and
`pthread_self` returns the pid. Every thread of a process reads and writes one
row of `TLS_VALUES` and gets one `pthread_t`: a key set in one thread is seen by
all of them, and `pthread_equal(pthread_self(), t)` holds for any two threads
of a process. Nothing says so; a C program that keeps per-thread state in a key
shares it.

The thread's own id is `toyos_abi::current_tid()`, read from the TCB, and each
thread has a TLS block of its own (`errno` lives in one:
`userland/libc/src/errno.rs`).

**Exit**: a key's value and `pthread_self` are the calling thread's, and a guest
C test that sets a key in one thread and reads it from another reds when they
are shared.
