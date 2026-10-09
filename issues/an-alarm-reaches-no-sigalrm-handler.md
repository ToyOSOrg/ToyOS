---
status: open
kind: defect
opened: 2026-09-30
---

# An alarm reaches no SIGALRM handler

libc's `alarm` (`userland/libc/src/alarm.rs`) arms one alarm per process and
answers the seconds the one it replaces had left, and when it is due a thread
of libc's ends the process with exit code 142, `SIGALRM`'s default action. A
handler for `SIGALRM` never runs: `sigaction` keeps none, and answers 0
whatever it is given. LLVM bounds its wait on a child with one: a `SIGALRM`
handler makes `wait4` answer `EINTR` (`llvm/lib/Support/Unix/Program.inc`,
`Wait`). POSIX gives `alarm` no refusal, and its `SIGALRM` reaches a handler
only through the signals libc imitates from stage 3 of
`issues/a-childs-end-is-an-event-and-a-parent-takes-its-children-down.md`
on; the handler waits on that stage.

The seconds left are held to their rule on the host
(`tests/libc-arch/src/alarm_requests.rs`); no guest case runs an alarm.

**Exit**: `alarm` arms and disarms `SIGALRM` as POSIX says, which a guest C
case shows: a handler installed without `SA_RESTART` runs, and a `wait4` on a
child that has not ended answers `EINTR`; and after `alarm(5)`, `alarm(0)`
answers from 1 to 5 and a second `alarm(0)` answers 0.
