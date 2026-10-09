---
status: open
kind: defect
opened: 2026-09-30
---

# An alarm reaches no SIGALRM handler

libc's `alarm` (`userland/libc/src/alarm.rs`) arms one alarm per process and
answers the seconds the one it replaces had left, and when it is due a thread
of libc's drops it if `SIGALRM` is ignored and otherwise ends the process with
exit code 142, `SIGALRM`'s default action. POSIX keeps the process in two of
those cases:

- **A handler.** `signal` and `sigaction` keep `SIGALRM`'s disposition, and a
  handler never runs. LLVM bounds its wait on a child with one: a `SIGALRM`
  handler makes `wait4` answer `EINTR` (`llvm/lib/Support/Unix/Program.inc`,
  `Wait`).
- **A mask.** `SIGALRM` blocked in every thread stays pending until a thread
  unblocks it, and libc ends the process at once. libc keeps each thread's
  mask in that thread alone (`userland/libc/src/pthread.rs`, `MASK`), and a
  thread Rust's std starts is none of libc's, so nothing can read whether
  every thread blocks it.

POSIX gives `alarm` no refusal, and its `SIGALRM` reaches a handler, or waits
pending on a mask, only through the signals libc imitates from stage 3 of
`issues/a-childs-end-is-an-event-and-a-parent-takes-its-children-down.md` on;
both wait on that stage, its owner.

What a due alarm does under each disposition is held on the host
(`tests/libc-arch/src/alarm_requests.rs`); `207_libc_names.c` reads the
disposition back and arms and disarms an alarm. No guest case lets one come
due.

**Exit**: a guest C case shows `SIGALRM` delivered as POSIX says: a handler
installed without `SA_RESTART` runs, and a `wait4` on a child that has not
ended answers `EINTR`; with `SIGALRM` blocked in every thread, a due alarm
leaves the process running and `sigpending` names it, until an unblock ends
the process with 142; and with it ignored, a due alarm leaves the process
running.
