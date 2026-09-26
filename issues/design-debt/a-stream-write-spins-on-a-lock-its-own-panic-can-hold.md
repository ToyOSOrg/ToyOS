---
status: open
kind: defect
opened: 2026-09-26
---

# A stream write spins on a lock its own panic can hold

Found in the review of PR #527. `toyos/src/log/stdio.rs`'s `Held` is a
userland spinlock around each stream's partial line, taken by every
`stdout`/`stderr` write to a log ring and by libc's fds 1 and 2
(`userland/libc/src/posix_io.rs`). Two ways it waits:

- **A thread that runs at a real-time priority spins on a preempted holder.**
  At smp 1 the holder cannot run until the spinner yields, and the spinner
  never does. soundd's mix thread is clear of it (it writes a lane of its own,
  `claim_lane`); any other real-time thread that prints is not.
- **A panic raised inside `Held::with` re-enters it.** The panic's message is
  a write to stderr, which takes the stream's `Held` again on the same thread
  and spins for good. What can panic inside it today is the stamp:
  `toyos_abi::clock::nanos_since_boot` asserts on the clock page's magic.

So "a write never waits" holds for the lanes and the shared ring's push, and
not for this path.

**Exit condition**: a stream write that cannot spin on a holder that is not
running — the partial line kept per thread, or a re-entry and a contended
holder answered by writing what the caller has as its own record — shown by a
guest test that panics inside a write and gets its message, and one that
prints from a real-time thread against a preempted holder at smp 1.
