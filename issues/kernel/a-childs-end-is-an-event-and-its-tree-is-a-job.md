---
status: open
kind: track
opened: 2026-09-29
---

# A child's end is an event on its handle, and its tree is a job

libc cannot start a child: `fork` and `execvp` answer `ENOSYS`, `waitpid`
`ECHILD` and `system` `-1` (`userland/libc/src/misc.rs`,
`userland/libc/src/stdio.rs`), and there is no `posix_spawn`. M2 and M4 of `issues/build/toyos-builds-itself.md` and the
exit of `issues/kernel/toyos-runs-on-arm64.md` need it: a compiler driver runs
its linker, cargo runs rustc, CMake and Ninja run compilers. **Nothing here is
built before the owner answers the questions below.**

What stays: a spawn answers a `Process` handle and is the child's whole
authority — the child holds what the slot map duplicates and the endowment
moves; a process has no parent (`kernel/src/loader/mod.rs`, `spawn`) and its
exit lives on the object, so no parent reaps it and no orphan is adopted; a pid
is a name the kernel never reissues (`kernel/src/id_map.rs`). What is missing:

- **An end is not an event.** `OP_WATCH` on a `Process` answers `NotSupported`
  (`kernel/src/object/ops.rs`, `read_watch`), so init parks a thread per serving
  service in `SYS_PROCESS_WAIT` (`userland/init/src/main.rs`,
  `close_when_it_ends`), sshd polls `try_wait` on a 1–25 ms backoff
  (`userland/sshd/src/main.rs`, `reap`), and a `waitpid(-1)` has nothing to
  wait on.
- **An end does not say how.** A kill publishes 137, a handle fault 139 and a
  CPU fault -1 (`kernel/src/process.rs`, both architectures' trap paths), codes
  a program can exit with too, and std's `ExitStatus::code()` is never `None`.
- **A kill ends one process.** A killed cargo leaves its rustc children running
  on the terminal's pipes, and there is no group to kill.

The design is Fuchsia's tasks with pidfd's readiness:

- **An end is readiness.** A `Process` handle is `READABLE` to `OP_WATCH` once
  its exit is published, as a pidfd reads readable on termination and Fuchsia
  queues `ZX_TASK_TERMINATED` on a port; seL4, with no process object, sends a
  thread's fault to the endpoint its TCB names — the same rule one level lower,
  what ends a task told on a capability its creator holds. The inbox is the
  port: one poll set holds pipes, connections and children.
- **An end says how:** exited with a code, killed, or faulted and how.
- **A job is the group.** Every process is in one job, fixed at spawn: the one
  its spawn names, else its spawner's, as a Linux child "is born into the cgroup
  that the forking process belongs to". Jobs nest; a kill of one ends everything
  in it and below it and admits nothing after (`zx_task_kill`); a job is
  `READABLE` while nothing in it lives (cgroup v2's `populated`). A process
  holds no handle to its own job unless given one.

std's direct spawn lands in the caller's job and a launch carries one, since a
child init spawns would otherwise land in init's; libc keeps pid → handle for
its own children and reaches no other pid; a shell runs each pipeline as a job
its terminal kills on Ctrl+C; init keeps a job per service and watches its
processes in its one poller. The kernel grows no signals, parent relation,
process group, session, controlling terminal, wait-any, pid-addressed call or
thread, and each new cost is paid by the thread that causes it: an exit posts
its process's watch and, emptying its job, that job's chain, capped by a depth
bound; a kill walks the killed subtree on the killer's syscall. What names a
child's image and working directory is the storage and isolation tracks'
(`issues/kernel/the-kernel-is-small-interrupts-post-and-threads-wait.md` step
5, `issues/isolation/every-program-sees-only-the-files-it-was-given.md` stage
1).

## Stages

1. **An end is an event** (Q1). The kernel answers `read_watch` and `has_data`
   for a `Process`, whose watch becomes an `Arc` as an `Acceptor`'s is, and
   `close_ends_polls` answers `false` for one: a holder's close ends no other
   holder's watch. init's waiter threads go. *Exit*: a guest test holds
   children on their stdin, as the `held` role of
   `tests/toyos-rust-tests/src/bin/process_lifecycle.rs` does, watches all of
   them in one poller and releases one at a time: each completion names the
   child just released, `READABLE`, and `try_wait` then reads its code; a watch
   on a child already gone completes at once; a kill completes one; after one
   of two handles to a held child closes, a non-blocking submit finds no
   completion. Negative control: the stage reverted whole, where the first
   watch answers `NotSupported`; mutation: `close_ends_polls` alone back at
   `true` reds the last arm. Oracle: pidfd_open(2)'s readiness.
2. **An end says how** (Q2). The claim in `toyos-proclife` and the record on
   `ProcessObject` carry exited, killed or faulted, the fault kinds being the
   classes the two crash reports print between them (`SEGFAULT`, `SIGILL`,
   `SIGFPE`, `SIGBUS`, `SIGTRAP`, `FATAL`); std's `ExitStatus` and the SDK
   decode it. *Exit*: a guest test reads exited 137 from a child that exits
   137, killed from one killed, and each fault kind its architecture raises
   from a child that commits it. Negative control: the stage reverted whole,
   whose code alone cannot tell the first two apart. Oracle: POSIX
   `<signal.h>`'s classes.
3. **libc starts and waits for children** (Q3). std's launcher-or-direct rule
   (`rust/library/std/src/sys/process/toyos.rs`, `Command::spawn`) moves into
   `toyos` and both call it, so a C and a Rust caller give one program the same
   authority. `posix_spawn`/`posix_spawnp` pass what POSIX says a child
   inherits — each descriptor libc holds without close-on-exec, as the file
   actions leave it — through the slot map. `waitpid`, `wait` and `wait4`
   (LLVM's `Support/Unix/Program.inc` waits with it; `rusage` from
   `SYS_PROCESS_STATS`) read libc's pid → handle map; `waitpid(-1)` watches every
   unwaited child on one inbox kept for the process — `poll` builds a 2 MiB one
   per call today (`userland/libc/src/posix_io.rs`) — and a child past its
   capacity is refused `EAGAIN`. libc gains the `ppoll`, `pselect`, `select`,
   `sigsuspend` and signal-set calls it lacks, which Q3's delivery needs.
   `system`, `popen` and `pclose` spawn `/system/bin/shell -c`. `fork` stays
   `ENOSYS` (Baumann et al., "A fork() in the road", HotOS 2019). *Exit*: C
   corpus files — a child with a piped stdout read to EOF through `poll`, then
   `waitpid`ed with its status; three children `waitpid(-1)` answers once each
   in the order they end, then `ECHILD`; a killed child `WIFSIGNALED` with
   `SIGKILL`; a `pselect` woken by `SIGCHLD`. Negative control: the stage
   reverted whole, where the first file does not link. Mutation: a
   `waitpid(-1)` that waits on its first child alone reds the order. Oracle:
   POSIX's `posix_spawn`, `waitpid` and `pselect`.
4. **Jobs** (Q4, Q5). The `Job` object, placement at spawn, `SYS_JOB_CREATE`,
   the kill on a job and its readiness; admission and the kill are decided in
   `toyos-proclife`. init starts each service in a job and a launch in the job
   its caller sent, in whichever crate holds launch resolution by then
   (`issues/isolation/the-supervisor-is-host-tested-and-owns-the-stop.md` stage
   2); the job rides the launch's handle batch, which three slots and five
   extras fill to eight today (`toyos/src/launch.rs`), so the extras drop to
   four. libc's `POSIX_SPAWN_SETPGROUP` makes a job and `kill(-pgid)` kills it.
   *Exit*: host — `toyos-proclife`'s interleavings of a spawn into a job racing
   its kill hold that nothing enters a killed job and that everything in it when
   the kill is claimed ends, and red under a mutation that admits into a killed
   job. Guest — A starts B in job J, B starts C and launches D; killing J ends
   B, C and D, each read as killed, J reads `READABLE`, and a spawn into J is
   refused `Gone`.
   Negative control: the stage reverted whole, where killing B, all A can
   name, leaves C and D running. Mutation: a kill that skips nested jobs leaves
   D. Oracle: `zx_task_kill`'s and cgroup v2's `cgroup.kill`.
5. **Job control.** The shell runs each pipeline as a job and hands its
   terminal a `MANAGE`-only duplicate of the foreground one over the terminal's
   `surface` port, whose connector the terminal already provides it; the
   terminal's translator turns Ctrl+C into that job's kill — a tty's `SIGINT`
   to the foreground group, with no tty in the kernel — and a closed window into
   the session job's kill. `&` is a job the shell watches. sshd kills a
   session's job when its connection goes (`end` kills the direct child today)
   and awaits exits through the tokio fork, which registers the handle as
   tokio's Linux reaper registers a pidfd; test-runner's deadline kills the
   running test's job. Stop and continue need a suspend primitive and are not
   proposed. *Exit*: a pipeline whose stage started a child is ended by
   Ctrl+C with none of it left and the prompt back; a dropped sshd session whose
   command started a child leaves neither; tokio's `Child::wait` returns with no
   loop polling `try_wait`. Negative control: today's terminal writes Ctrl+C
   into the pipe and the pipeline runs on.

## Owner questions

**Q1. An end as readiness** (stage 1). `OP_WATCH` with `READABLE` on a
`Process` handle, needing `Rights::WAIT` as every watch does, completes
`READABLE` once the exit is published, at registration if it already is; a
`WRITABLE`-only watch answers `-NotSupported` as today, and closing one handle
ends no watch on the process. No syscall, number or constant is added.
*Recommended.* Rejected: an `EXITED` bit, a second word for "the next wait
answers at once", which `READABLE` says on every other object and a pidfd says
with `EPOLLIN`; and a wait-any syscall over a handle array, a second blocking
mechanism beside the inbox.

**Q2. What an end says** (stage 2). `SYS_PROCESS_WAIT` (108) keeps its number,
arguments, right (`WAIT`) and errors; its answer carries `code` in bits 0–31
and `how` in bits 32–39. `how` 0 is exited, `code` the program's own; 1 is
killed, `code` 0; 2 is faulted, `code` the kind: 1 memory, 2 instruction, 3
arithmetic, 4 bus, 5 breakpoint, 6 another CPU exception, 7 a handle it did not
hold. Bits 40–63 stay zero, clear of the error range. `KILLED_EXIT_CODE`,
`HANDLE_FAULT_EXIT_CODE` and the fault path's -1 go; an abort stays the
program's own exit code, 134 from std and from libc. *Recommended.* Rejected:
the shell conventions in the `i32`, which read a program's `exit(137)` as a
kill; and an out-pointer struct, a copy-out for what one return word carries,
as `SYS_PIPE`'s carries two handles.

**Q3. `SIGCHLD` is libc's** (stage 3; no ABI). libc raises it from its own
children's ends and runs the handler only inside a libc call that blocks with
it unblocked — `ppoll`'s and `pselect`'s mask, `poll`, `select`, `sigsuspend`
— which then answers `EINTR`; `sigaction` records handlers rather than
answering `0` for nothing as today. Ninja blocks `SIGCHLD` except inside
`ppoll`/`pselect` (v1.13.1 `src/subprocess-posix.cc`), where this is POSIX's
own delivery, and libuv without kqueue learns of a child from a handler that
writes its self-pipe (`src/unix/process.c`, `src/unix/signal.c`), which an
`EINTR` serves. A program that waits for `SIGCHLD` without blocking in libc
never gets it. *Recommended.* Rejected: a ToyOS backend in each such program —
libuv's kqueue `EVFILT_PROC` shape, which this handle's readiness also fits —
a fork per program for what one libc routine serves.

**Q4. Jobs** (stage 4).
- A fourteenth object kind, `Job`. The root job is made at boot and holds init;
  no handle to it exists.
- `SYS_JOB_CREATE` = 124, `(parent: RawHandle) -> RawHandle`: `parent` is
  `HANDLE_INVALID` for the caller's own job, else a job carrying
  `Rights::WRITE`; it answers a new job inside it carrying
  `DUP|TRANSFER|WAIT|WRITE|MANAGE`. `Gone` if `parent` is killed;
  `ResourceExhausted` past `MAX_JOB_DEPTH` (32, a choice, not a measurement) or
  a full table; `PermissionDenied` without `WRITE`; a handle not held, or not a
  job, ends the caller as everywhere.
- `SpawnArgs` gains `job: RawHandle` and `_pad: u32`, 96 bytes to 104:
  `HANDLE_INVALID` is the caller's own job, else a job carrying `WRITE`; `Gone`
  if it is killed, refused before anything moves.
- 109 takes a `Process` or a `Job` carrying `MANAGE` and is renamed
  `SYS_TASK_KILL`, its arguments unchanged, as 89 was renamed. On a job it ends
  every process in it and below it killed, admits nothing after, and answers
  once every retire is posted, waiting for none; `Ok` for one already killed.
- `OP_WATCH` `READABLE` on a job (`WAIT`) completes while nothing in it or below
  it lives; closing one handle ends no watch on it.

*Recommended.* Rejected: a process killed when its last handle closes,
Capsicum's `pdfork` default without `PD_DAEMON`, because Rust documents that a
dropped `Child` "will continue to run"; a kernel-kept parent tree with a
kill-descendants flag, the relation ToyOS retired with `SYS_KILL` (65); and a
`SYS_JOB_KILL` of its own, a second verb for one right.

**Q5. Retire `SYS_PROCESS_OPEN` (110)** with stage 4, and `MANAGE`'s meaning on
a `SysCap` with it. It is the one call where a pid becomes authority, and
nothing but its own test reaches it (`toyos/src/syscap.rs`'s `open_process` has
no caller); a job per service gives init every tree it started without a pid.
*Recommended.* Rejected: keeping it for a debugger or a `kill <pid>` tool,
callers that do not exist — the ruling threads-as-objects got
(`issues/kernel/the-capability-end-state-is-twelve-answers.md`, question 9).

## Sources

- pidfd_open(2), https://man7.org/linux/man-pages/man2/pidfd_open.2.html
- `zx_task_kill`, https://fuchsia.dev/fuchsia-src/reference/syscalls/task_kill;
  `zx_object_wait_async`, https://fuchsia.dev/fuchsia-src/reference/syscalls/object_wait_async
- cgroup v2, https://docs.kernel.org/admin-guide/cgroup-v2.html
- pdfork(2), https://man.freebsd.org/cgi/man.cgi?query=pdfork&sektion=2
- seL4 fault handlers, https://docs.sel4.systems/Tutorials/fault-handlers.html
- `std::process::Child`, https://doc.rust-lang.org/std/process/struct.Child.html
- tokio, https://github.com/tokio-rs/tokio/blob/master/tokio/src/process/unix/pidfd_reaper.rs
- Ninja, https://github.com/ninja-build/ninja/blob/v1.13.1/src/subprocess-posix.cc
- libuv, https://github.com/libuv/libuv/blob/v1.x/src/unix/process.c
- LLVM, https://github.com/llvm/llvm-project/blob/llvmorg-21.1.0/llvm/lib/Support/Unix/Program.inc
- Baumann et al., "A fork() in the road", https://dl.acm.org/doi/10.1145/3317550.3321435
