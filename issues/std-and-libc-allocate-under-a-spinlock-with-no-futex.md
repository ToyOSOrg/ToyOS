---
status: open
kind: defect
opened: 2026-10-04
---

# std's and libc's allocators spin on one flag, so a waiter that outranks the holder spins in its place

Every allocation a ToyOS program makes goes through one `dlmalloc` behind a
process-wide `AtomicI32` that a waiter takes with `swap(1, Acquire)` in a
`spin_loop()` loop, never sleeping on a futex:

- std's: `sdk/std/sys/alloc.rs`, lines 57–72 (`LOCKED`, `lock`, `DropLock`), taken by `alloc`,
  `alloc_zeroed`, `dealloc` and `realloc`, lines 74–96.
- libc's, in a program linked without std: `userland/libc/src/lib.rs`, lines
  174–188, taken by `LibcAllocator`'s `alloc`, `dealloc` and `realloc`, lines
  192–207. Beside std (`std-runtime`), libc's C allocator calls std's
  (`userland/libc/src/memory.rs`).

A holder that is preempted, or that is inside the `mmap` or `munmap` syscall
`dlmalloc` makes under the lock through its backing allocator, leaves every
other thread of its process that allocates spinning through its slice. A
real-time thread outranks a fair one by right
(`issues/cpu-time-is-a-band-and-not-a-reservation.md`), so a real-time
waiter on the holder's CPU keeps a fair holder off that CPU while it spins:
priority inversion. How long such a spin lasts, and whether it ends before the
holder is run elsewhere, is unmeasured, and so is whether two threads ever meet
on this lock: `thread::spawn` allocates, and several programs allocate from more
than one thread, among them `userland/sshserver`, `userland/supervisor` and
`userland/soundserver`.

The futex both need is there: libc's `futex_lock` and `futex_unlock`
(`userland/libc/src/pthread.rs`, lines 116–131), and std's `sync::Mutex`, which
is the futex mutex on ToyOS (`library/std/src/sys/sync/mutex/mod.rs`, over
`sdk/std/sys/pal/futex.rs`).

Owner track: `issues/toyos-has-its-own-allocator.md`, whose std front
replaces std's allocator; it does not rule whether this lock is fixed on
`dlmalloc` first. Owner: the orchestrator.

**Exit**: neither file's allocator spins: a contended allocation waits on a
futex and a release with waiters wakes one, or std's front of ToyOS's
allocator has replaced both.
