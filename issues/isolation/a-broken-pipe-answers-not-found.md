---
status: open
kind: defect
opened: 2026-08-10
---

# A broken pipe reaches a Rust caller as `ErrorKind::Other`

**The kernel half is closed.** `ops::write_pipe` answers `SyscallError::Gone`
for a pipe whose readers are all gone (`kernel/src/object/ops.rs`), soundd's and
netd's death detectors key on that word, `userland/libc`'s `set_errno` maps it
to `EPIPE`, and `connect_before_serve`, `kill_while_blocked`, `handle_transfer`,
`compositor_stall` and `pipe_flag_forgery` all assert it.

**What is left is the std fork, and only the std lane can do it.**
`rust/library/std/src/sys/pipe/toyos.rs`'s `to_io_error` names `NotFound`,
`PermissionDenied` and `WouldBlock` and sends everything else — `Gone` included
— to `io::ErrorKind::Other`. `rust/library/std/src/sys/stdio/toyos.rs`'s
`to_io_error` is worse: it ignores its argument entirely and answers `Other` for
every error there is.

So a Rust program writing into a pipe whose reader has exited gets
`ErrorKind::Other`, where POSIX gives `EPIPE` and every other Rust target gives
`ErrorKind::BrokenPipe`. That is uninformative rather than false — the word it
replaced said the pipe did not exist, which is a different fact with different
remedies — but it is still not an answer a caller can act on.

## What closing it takes

`Gone => io::ErrorKind::BrokenPipe` in `sys/pipe/toyos.rs`, and a `to_io_error`
in `sys/stdio/toyos.rs` that reads its argument at all. Both are in the rust
submodule, which needs an exclusive machine window, so this is owed to the std
lane and to nothing else.

The instrument is a guest arm: a `std::process::Child` whose stdin write must
answer `ErrorKind::BrokenPipe` after the child has exited, measured against
libc's `write(2)` into the same shape setting `errno` to `EPIPE` — which it
already does, so libc is the differential the std arm is judged by.
