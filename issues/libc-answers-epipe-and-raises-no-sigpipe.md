---
status: open
kind: defect
opened: 2026-10-09
---

# libc answers EPIPE and raises no SIGPIPE

POSIX's `write` to a pipe or socket whose reader is gone, and its `send` on a
stream that can send no more, fail `EPIPE` and also send the calling thread
`SIGPIPE`, whose default action ends the process; `send` raises none only with
`MSG_NOSIGNAL`. ToyOS's libc fails `EPIPE` and raises nothing
(`userland/libc/src/posix_io.rs`, `SyscallError::Gone`): it has no signals
(`signal`, `sigaction` and `raise` in `userland/libc/src/misc.rs` do nothing),
and none of those
`issues/a-childs-end-is-an-event-and-a-parent-takes-its-children-down.md`
has it imitate is `SIGPIPE`. So every write behaves as if `MSG_NOSIGNAL` were
given, and a C program that relies on the default action to stop writing into a
closed pipe runs on, and stops only if it reads the error. `send` on a stream answers `EIO` for every refusal today; the libc
change the move carries for
`issues/a-netstack-client-cannot-tell-a-reset-from-the-peers-fin.md` answers
`EPIPE` after `shutdown(SHUT_WR)`, and raises nothing either.

Whether libc raises `SIGPIPE` on these, or states the departure as it states
others, is a decision of libc's design that nobody has made.

Read from the code, not measured.

**Exit condition**: the decision is made and recorded at `posix_io.rs`, and a
guest C case asserts it on both paths, a `write` into a pipe whose reader is
gone and a `send` after `shutdown(SHUT_WR)`: with `SIGPIPE` raised, the default
action ends the process, an ignored one leaves `EPIPE` and `MSG_NOSIGNAL`
raises none; with the departure kept, each answers `EPIPE` and the process
lives on.

**Owner**: `userland/libc`.
