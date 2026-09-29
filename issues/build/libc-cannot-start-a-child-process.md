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

**Who needs it.** ninja and cmake/libuv launch every job through
`posix_spawn` or `fork`+`exec` with piped stdout and `poll`; LLVM's
`Support/Program.inc` does the same when clang runs `ld.lld`. M2 and M4 of
`issues/build/toyos-builds-itself.md` cannot pass without it, and neither can
the LLVM exit of `issues/kernel/toyos-runs-on-arm64.md`.

**Constraint.** The kernel ABI stays capability-shaped and POSIX lives in
`userland/libc` under its relaxed rules. `SYS_SPAWN` with a slot map is the
primitive libc builds on, so no new syscall is owed for `posix_spawn`, its
file actions (`dup2`, `close`, `open`) resolving to a slot map, and `waitpid`
reading the `pid -> Process handle` map `waitpid`'s doc comment already
names. `fork` has no capability-shaped meaning and stays refused; a caller that
needs it is ported to `posix_spawn`. How libc reaches the launcher that std's
spawn asks for programs with a manifest row is not decided here and must match
std's answer.

**Exit**: a guest C test that `posix_spawn`s a child with a piped stdout,
reads it to EOF through `poll`, `waitpid`s it and checks its exit status; and
clang driving `ld.lld` on ToyOS.
