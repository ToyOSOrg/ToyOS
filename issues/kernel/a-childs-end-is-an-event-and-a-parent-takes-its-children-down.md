---
status: open
kind: track
opened: 2026-09-29
---

# A child's end is an event, and a parent takes its children down

libc cannot start a child: `fork` and `execvp` answer `ENOSYS`, `waitpid`
`ECHILD` and `system` `-1` (`userland/libc/src/misc.rs`,
`userland/libc/src/stdio.rs`), and there is no `posix_spawn`. M2 and M4 of
`issues/build/toyos-builds-itself.md` and the exit of
`issues/kernel/toyos-runs-on-arm64.md` need it. Nor is a child's end an event
(init parks a thread per service, sshd polls `try_wait`), nor does it say how;
a kill ends one process, and nothing asks a program to quit. Stages 2, 3, 6
and 7 wait on
`issues/kernel/the-child-process-track-waits-on-the-owners-rulings.md`.

**Constraints.** libc is built on ToyOS and never ToyOS on libc (owner,
2026-09-30: "toyos moderness and anti legacy may never be compromised because
of libc. no legacy"). One mechanism (owner: "i dont want multiple
mechanisms"): the `Process` handle is the only control, and the kernel grows
no job object, process group, POSIX session, controlling terminal, signal,
wait-any, pid-addressed call or thread. What names a child's image and working
directory is `issues/isolation/every-program-sees-only-the-files-it-was-given.md`'s.

**Ruled** (owner, 2026-09-30):

- A child's end is readiness on its handle: `OP_WATCH` `READABLE` on a
  `Process` (`WAIT`) completes once the exit is published, closing one handle
  ends no other's watch, and the ABI gains nothing.
- `SYS_PROCESS_OPEN` (110) is retired now, as stage 0.
- Every process has one parent and any number of children, and a parent's end
  — exit, crash or kill — kills its whole subtree at once; quitting gracefully
  is the parent's job before it exits, never the kernel's. The kernel starts
  init and init starts everything else, with no rule for services: a server
  is init's child and lives as long as init. Each login, desktop or SSH, is a
  session process under init that parents everything its user starts; an app
  uses a server through handles and is never its child; logging out ends the
  session. "Keep running after I close this" asks init, through `launcher`, to
  be the parent, and nothing else reparents. A kill is cleanup without
  cooperation, run on the causing path in bounded steps. init never dies and
  stays tiny.

## Stages

0. **`SYS_PROCESS_OPEN` goes** (ruled). Deleted: `sys_process_open` and every
   name only it reaches — `process_open`, `SysCap::open_process`, `MANAGE` in
   init's `SysCap`, the `reopenable` column, `process::process_object`,
   `reopen_selftest` and `sched::kthread::open_selftest` with their actuator,
   guest test, judge and rows — and
   `process_lifecycle`'s pid-open arm, its only caller; 110 enters
   `retired_syscalls!`. `issues/kernel/the-capability-end-state-is-twelve-answers.md`
   and `issues/diagnostics/the-kernel-keeps-nothing-it-enumerates.md`, which
   argue from it, change in the same landing. *Exit*: a call of 110 answers
   `NotSupported` and the log names it retired; `git grep` finds no deleted
   name outside `toyos-symbols/tests/fixtures/input-test.bin`, a frozen binary
   whose symbols are test data. Negative control: the stage reverted whole,
   where `sys_process_open` answers 110. Oracle: rustc's name resolution,
   which fails the build of any caller left.
1. **An end is an event** (ruled). `read_watch` and `has_data` answer for a
   `Process`, whose watch becomes an `Arc` as an `Acceptor`'s is, and
   `close_ends_polls` answers `false` for one; init's waiter threads go.
   *Exit*: children held on their stdin (`process_lifecycle`'s `held` role),
   watched in one poller and released one at a time, each completion naming the
   child just released, whose code `try_wait` then reads; a watch on a child
   already gone completes at once; a kill completes one; after one of two
   handles to a held child closes, a non-blocking submit finds nothing.
   Negative control: the stage reverted whole (`NotSupported`); mutation:
   `close_ends_polls` at `true` reds the last arm. Oracle: pidfd_open(2).
2. **An end says how** (Q2). *Exit*: exited 137, killed, and each fault kind
   the architecture raises, each read from a child that did it. Negative
   control: the stage reverted whole, where the first two read alike. Oracle:
   POSIX `<signal.h>`'s classes.
3. **libc starts and waits for children** (Q3; blocked on
   `issues/isolation/a-childs-stdio-handle-is-not-the-one-command-named.md`).
   `posix_spawn` and `posix_spawnp` with file actions and attributes, routed
   by std's launch-or-spawn rule moved into `toyos`; a descriptor table with
   close-on-exec; `waitpid`, `wait`, `wait4`; `ppoll`, `sigpending` and the
   signal-set calls; `environ`, `setenv`, `unsetenv`; `kill` with `SIGKILL`
   for a child or a `-pgid` of its children, 0 as a probe, `EINVAL` for other
   signals until stage 6; `system`, `popen` and `pclose` through
   `/system/bin/shell -c`. No `select`: a descriptor is a handle and passes
   `FD_SETSIZE`. `poll` skips a negative descriptor, as POSIX says. libc's own
   thread watches at most 255 unwaited children on its own inbox, the 256th
   watch being stage 6's notice, and wakes a blocked `poll` through one pipe,
   so `poll` takes at most 255 descriptors and children never count against
   them. Owed elsewhere: `/bin/sh`
   (`issues/build/ninja-runs-every-command-through-a-bin-sh-toyos-does-not-have.md`),
   `/dev/null` (`issues/filesystem/there-is-no-dev-null.md`) and
   `dlsym(RTLD_DEFAULT, …)`
   (`issues/build/libc-dlsym-rtld-default-panics-as-unimplemented.md`).
   *Exit*, C corpus files, the mutation that reds an arm after it: a piped
   stdout read to EOF through `poll`, then `waitpid`; `waitpid(-1)`
   answering three children once each in the order they end, then `ECHILD`
   (waiting on the first alone); a killed child `WIFSIGNALED` `SIGKILL`; the
   parent's stdout marked `FD_CLOEXEC`, a 0 that `addopen` opened `O_CLOEXEC`,
   and a 1 that `adddup2` made and `addclose` closed, each absent in the child
   (`F_SETFD`, `O_CLOEXEC` or `addclose` ignored); the 256th unwaited child
   refused `EAGAIN` (no capacity check, which panics in the poller), and
   beside 255 of them a `poll` of 255 descriptors answering (children watched
   in `poll`'s own poller, which panics); under `SIG_IGN`, 256 children each
   ending before the next all spawned, then `ECHILD` (ended children kept); a
   `ppoll` unblocking `SIGCHLD` answering `EINTR` once a child ends, and a
   second, the child unwaited, answering its readable pipe, and with nothing
   ready blocking until a second child writes (`SIGCHLD` raised again at every
   blocking call, or at every one that finds nothing ready); a negative
   descriptor beside a pipe, the pipe answering (passed to the kernel, which
   ends the caller). Negative control: today's libc, against which the corpus
   does not link. Oracle: POSIX's `posix_spawn`, `waitpid` and `poll`.
4. **A parent takes its children down** (ruled). A spawn's parent is its
   spawner. A launch names its parent: the caller, by a copy of the handle to
   itself every process starts holding (`WRITE`, `DUP`, `TRANSFER`), which
   takes one of the batch's five extras (`toyos/src/launch.rs`); or init, the
   one way to outlive a starter, which the shell gains a way to ask for. init
   spawns under the place, which `SpawnArgs` gains (96 bytes to 104); a place
   of the wrong type answers `InvalidArgument` rather than ending init, as
   `SYS_NAMESPACE_BUILD`'s connector does, because a peer sent it. The kernel
   keeps each process's parent and children on its table entry, read by the
   end walk alone and answered by no syscall, and refuses `ResourceExhausted`
   a spawn that would put a process more than 64 below init. Every end —
   exit, kill, CPU fault, handle fault — closes admission at its top and
   claims its subtree on the ending or killing thread, one process per
   table-lock hold; a spawn under a claimed process answers `Gone`. An end is
   published once its teardown is done and every child's end is, by a count
   admission raises and a child's publication or refused insert lowers, so
   the climb from the last teardown, run with preemption off, is at most 64
   publications. Decided in `toyos-proclife`; init's part goes where launch
   resolution is by then
   (`issues/isolation/the-supervisor-is-host-tested-and-owns-the-stop.md`
   stage 2). *Exit*: host — `toyos-proclife`'s interleavings of a spawn racing
   its place's kill land nothing under it, end all under it and publish it in
   every one; a spawn under an unrelated process lands between two claims of
   one walk; a chain is refused 65 below init. Guest — A starts B, B starts C
   and launches D: killing B ends all three, read as killed, and A's watch on
   B completes after C's and D's; so do B's exit, a CPU fault and a handle
   fault in B; a spawn or launch under B after its kill answers `Gone`; a
   chain spawned until refused answers `ResourceExhausted` and dies whole with
   its first; a launch whose place is a pipe is refused and init answers the
   next; a `MANAGE`-only place answers `PermissionDenied`; sshd launched from
   a shell dies with it, and with init as its parent outlives it. Negative
   control: the stage reverted whole, where C and D outlive B. Mutations: a
   walk that skips a launched child (D); a publication before the children's
   (A's watch); a walk from `sys_exit` alone (the faults); the walk in one
   lock hold (the interleaved spawn); the count kept on a refused insert (B
   unpublished); the place looked up as any handle (init ends); no depth check
   (the chain). Oracle: cgroup v2's `cgroup.kill` and `cgroup.max.depth`.
5. **A login is a session under init** (ruled). init starts a session process
   per login — the desktop's at boot, one per SSH connection at sshd's request
   — and the compositor and sshd start a login's programs under it, through a
   place the session hands them, never under themselves; sshd ends a
   connection's session when it drops. Its namespace is
   `issues/filesystem/a-user-is-a-home-tree-and-a-login-row.md`'s login row.
   *Exit*: killing the desktop session ends every program the compositor
   started and leaves the compositor running; killing the compositor ends none
   of them; a dropped SSH connection ends its session and all under it.
   Negative control: the stage reverted whole, where the compositor's programs
   die with it. Oracle: systemd-logind, whose session scope ends every process
   of a login.
6. **A program is asked to quit** (Q6). *Exit*: quit, a child watching its
   notice writes its file and exits 0; one that never watched reads killed;
   one that watches and never ends is killed at its parent's deadline; a Rust
   child whose `ctrlc` handler writes its file and exits 0 does so; a C child
   with LLVM's handlers — remove its output, restore the default, raise — that
   computes without calling libc, asked with interrupt, ends at once with its
   output gone and code 130; a C child whose `SIGHUP` handler reloads runs on
   after a hang-up and exits on a terminate; a quit that arrives while
   `SIGINT` is blocked and `ppoll` answers a ready descriptor shows in
   `sigpending`, and the next `ppoll` runs the handler and answers `EINTR`. Negative control: the stage
   reverted whole, where a parent can only kill. Mutations: a quit notifying a
   process that never watched (it runs on); `ctrlc` parked as today (killed);
   handlers run only in blocking libc calls (the LLVM child to the hang
   ceiling); a quit without its reason (the reload exits). Oracle: POSIX's
   signal actions (XSH 2.4.3).
7. **Job control** (Q6). The shell hands its terminal a `MANAGE`-only
   duplicate of each process of the foreground line over its `surface` port;
   the terminal turns Ctrl+C into their interrupt and a second Ctrl+C into
   their kill, and before it closes asks its shell's tree to hang up and kills
   what is left at its deadline; sshd does that to a dropped connection's
   session, and init at shutdown and test-runner at a test's deadline ask with
   terminate. `&` is a line the shell watches. sshd awaits exits through a
   ToyOS `tokio/src/process` arm in the tokio fork, shaped as its Linux pidfd
   arm (`process/unix/pidfd_reaper.rs`). Stop and continue need a suspend
   primitive and are not proposed. *Exit*: Ctrl+C ends a pipeline whose stage
   started a child, none of it left and the prompt back, and a second ends a
   stage that ignores the interrupt; a dropped SSH
   connection whose command started a child leaves neither; tokio's
   `Child::wait` returns with no loop polling `try_wait`. Negative control:
   today's terminal and shell, where the pipeline runs on. Oracle: POSIX's
   INTR, a `SIGINT` "sent to all processes in the foreground process group"
   (XBD 11.1.9), and tokio's `tests/process_smoke.rs`.
