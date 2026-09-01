---
status: open
kind: defect
opened: 2026-07-30
---

# Std leaks a whole thread stack on every `thread::spawn`

`rust/library/std/src/sys/thread/toyos.rs` allocates the stack with
`alloc::alloc` (2 MiB minimum), hands its base to `SYS_THREAD_SPAWN`, and never
records the pointer. `Thread` holds only a tid and has no `Drop`, `join` does not
free it, and the trampoline cannot — it is standing on it. So every spawned
thread costs 2 MiB of heap for the life of the process, which dlmalloc serves
from a dedicated `mmap` above its 256 KiB threshold: one leaked 2 MiB kernel
region per spawn, walking the address space downwards.

Found while testing thread-exit TLS release, where the drift swamped the signal
(the test now drives `SYS_THREAD_SPAWN` directly on a reused stack). It also
makes any per-process memory measurement across a thread-spawning workload wrong.

**A fix owes both halves, and the obvious one closes only the first.** Putting a
base/layout pair on `Thread` and freeing it in `join` after the tid is reaped
covers the joined thread. It leaves the detached thread exactly as it is:
`Thread::join` takes `self`, `Thread` has no `Drop`, and a `JoinHandle` dropped
without joining detaches — so on that path nobody is left holding the base.

**The kernel is already told the base and does not own it.** `stack_base` is the
fourth argument of `SYS_THREAD_SPAWN`; `sys_thread_spawn` refuses it above
`stack_ptr` and `spawn_thread` stores it as `user_stack_base`/`user_stack_size`
on the **`ThreadData`** (`kernel/src/process.rs:565`, `:569-570`) — per-thread,
not on the process. The only reader is `SYS_STACK_INFO`, which copies the pair
back out to userland. So the record dies with the thread and the memory does
not: nothing frees the stack, and nothing makes the mapping the thread's to
release. The detached half's answer is therefore a design question — the kernel
reclaiming a thread stack it already knows, or std refusing to detach a thread
whose stack it allocated — and not a missing line in `join`.

**It cannot be closed without a plateau to measure**, which is
`issues/kernel/a-processs-memory-is-a-byte-total-that-reads-zero-under-contention.md`:
a single before/after pair cannot distinguish a leak from a deferred release, and
the plateau has to cover the detached path too.
