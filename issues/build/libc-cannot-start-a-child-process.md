---
status: open
kind: track
opened: 2026-09-29
---

# libc cannot start a child process

`userland/libc/src/misc.rs` answers `fork` and `execvp` with `ENOSYS`,
`waitpid` with `ECHILD`, and `system` (`stdio.rs`) with `-1`; there is no
`posix_spawn`. A C program on ToyOS cannot run another program. Rust can:
`rust/library/std/src/sys/process/toyos.rs` builds a `SpawnArgs` (argv, env,
cwd, a slot map that duplicates stdio and pipe ends into the child) and calls
`toyos_abi::syscall::spawn`, then waits with `SYS_PROCESS_WAIT` on the returned
`Process` handle. `pipe`, `dup2` and `poll` already exist in
`userland/libc/src/posix_io.rs`.

**Who needs it.** M2 and M4 of `issues/build/toyos-builds-itself.md` cannot
pass without it, and neither can the LLVM exit of
`issues/kernel/toyos-runs-on-arm64.md`.

**Open ABI question.** `SYS_PROCESS_WAIT` waits on one handle, blocking or
`WNOHANG`. `toyos/src/poller.rs` does not watch process handles, and libc
`sigaction`, `kill` and `raise` are no-ops, so there is no `SIGCHLD`. Waiting
on any child (`waitpid(-1)`) or a `SIGCHLD` plus `ppoll` reap loop therefore
has no event to wait on. Whether the kernel offers child exit as a pollable
event is open and owed a discussion.

**Constraint.** The kernel ABI stays capability-shaped and POSIX lives in
`userland/libc` under its relaxed rules.

**Exit**: a guest C test that `posix_spawn`s a child with a piped stdout,
reads it to EOF through `poll`, `waitpid`s it and checks its exit status; and
clang driving `ld.lld` on ToyOS.
