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
`issues/kernel/toyos-runs-on-arm64.md` need it. Stages 2, 3, 6 and 7 wait on
`issues/kernel/the-child-process-track-waits-on-the-owners-rulings.md`, and
are written as its questions recommend.

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
2. **An end says how** (Q2, Q6d). `SYS_PROCESS_WAIT` (108) keeps its number,
   arguments, right and errors. Bit 32 of its answer is 1 if the kernel ended
   the process, and bits 0–31 are then why, never 0 — 1 killed, 2 a memory
   access its mappings refuse, 3 an instruction it may not run, 4 an
   arithmetic trap, 5 a misaligned access, 6 a breakpoint instruction, 7 a
   handle it does not hold, each the same act on x86-64 and AArch64 — and
   otherwise its exit code; bits 33–63 stay 0, clear of the error range. 137,
   139 and -1 go; an abort stays exit code 134, from std and from libc. std's
   `code()` answers `None` for a process the kernel ended and a ToyOS
   `ExitStatusExt` reads why; libc's `waitpid` reports it `WIFSIGNALED` with
   `SIGKILL`, `SIGSEGV`, `SIGILL`, `SIGFPE`, `SIGBUS`, `SIGTRAP` or `SIGSYS`
   for 1 to 7. *Exit*: exited 137, killed, and each fault kind the
   architecture raises, each read from a child that did it. Negative control:
   the stage reverted whole, where the first two read alike. Oracle: POSIX
   `<signal.h>`'s classes.
3. **libc starts and waits for children** (Q3a–Q3c; blocked on
   `issues/isolation/a-childs-stdio-handle-is-not-the-one-command-named.md`).
   `posix_spawn` and `posix_spawnp` with file actions and attributes, routed
   by std's launch-or-spawn rule moved into `toyos`; a descriptor table with
   close-on-exec, a child getting descriptors 0–2 and exactly what its file
   actions name; `waitpid`, `wait`, `wait4`; `ppoll`, `sigpending` and the
   signal-set calls; `environ`, `setenv`, `unsetenv`; `kill` with `SIGKILL`
   for a child or a `-pgid` of its children, 0 as a probe, `EINVAL` for other
   signals until stage 6; `system`, `popen` and `pclose` through
   `/system/bin/shell -c`. No `select`: a descriptor is a handle and passes
   `FD_SETSIZE`. `poll` skips a negative descriptor, as POSIX says. libc's own
   thread watches at most 255 unwaited children on its own inbox, the 256th
   watch being stage 6's notice, and wakes a blocked `poll` through one pipe,
   so `poll` takes at most 255 descriptors and children never count against
   them. It raises `SIGCHLD` once per end of a child libc started. A handler
   for any signal libc imitates runs inside a `ppoll` under its mask, or a
   `poll`, of a thread that leaves the signal unblocked, and that call answers
   `EINTR`; while no such thread is in one, at once on libc's own thread; and
   while every thread blocks the signal, it stays pending, as `sigpending`
   reports, until one unblocks it. Under `SIG_IGN` or `SA_NOCLDWAIT` an ended
   child is dropped unwaited. `sigaction` records handlers, where today it
   answers `0` and records nothing. Owed elsewhere: `/bin/sh`
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
   spawner. A launch names its parent in its request, by one of two words:
   the caller, whose place is a copy of the handle to itself every process
   starts holding (`WRITE`, `DUP`, `TRANSFER`), carried as one of the batch's
   five extras (`toyos/src/launch.rs`); or init, the one way to outlive a
   starter, which std's ToyOS `CommandExt` asks for and the shell gains a way
   to use. init refuses a request that says neither, and starts nothing. init
   spawns under the place, which `SpawnArgs` gains (112 bytes to 120). The
   place and `SYS_NAMESPACE_BUILD`'s connector are each a handle a peer sent,
   and one lookup resolves both, answering a wrong type `InvalidArgument`
   rather than ending the caller: `HandleError::WrongType` declares that
   exception once, for that lookup, and the connector's own match in
   `kernel/src/syscall/ipc.rs` goes. The kernel keeps each process's parent,
   children and depth on its table entry, read by the end walk alone and
   answered by no syscall. A spawn that would put a process more than
   `toyos_proclife::MAX_DEPTH` (64) below init, counted from the place, is
   refused `ResourceExhausted`, and the kernel logs a depth refusal naming
   that depth. Every end — exit, kill, CPU fault, handle fault — closes
   admission at its top and claims its subtree on the ending or killing
   thread, one process per table-lock hold, calling `scheduler::yield_now`
   between two claims, with nothing held, whenever a reschedule is owed: a
   subtree's width costs the thread that ends it and no other thread's
   latency. A spawn under a claimed process answers `Gone`. An end is
   published once its teardown is done and every child's end is, by a count
   admission raises and a child's publication or refused insert lowers, so
   the climb from the last teardown, run with preemption off, is at most
   `MAX_DEPTH` publications. Decided in `toyos-proclife`; init's part goes
   where launch resolution is by then
   (`issues/isolation/the-supervisor-is-host-tested-and-owns-the-stop.md`
   stage 2). *Exit*: host — `toyos-proclife`'s interleavings of a spawn racing
   its place's kill land nothing under it, end all under it and publish it in
   every one; a spawn under an unrelated process lands between two claims of
   one walk; a chain is refused at `MAX_DEPTH` + 1 below init. Guest — A
   starts B, B starts C and launches D: killing B ends all three, read as
   killed, and A's watch on B completes after C's and D's; so do B's exit, a
   CPU fault and a handle fault in B; a spawn or launch under B after its
   kill answers `Gone`; a chain alternating spawn and launch, each process
   starting the next, stops where the kernel's depth refusal names
   `MAX_DEPTH` + 1, and dies whole with its first; a launch that neither
   carries a place nor asks for init is refused and nothing starts; a launch
   whose place is a pipe is refused and init answers the next; a
   `MANAGE`-only place answers `PermissionDenied`; sshd launched from a shell
   dies with it, and with init as its parent outlives it. Negative control:
   the stage reverted whole, where C and D outlive B. Mutations: a walk that
   skips a launched child (D); a publication before the children's (A's
   watch); a walk from `sys_exit` alone (the faults); the walk in one lock
   hold (the interleaved spawn); the count kept on a refused insert (B
   unpublished); the place looked up as any handle (init ends); no depth
   check, and a depth counted from the spawner rather than the place (the
   chain, which no depth refusal stops); a launch with no place spawned under
   init (it starts). Oracle: cgroup v2's `cgroup.kill` and
   `cgroup.max.depth`.
5. **A login is a session under init** (ruled; blocked on three decisions of
   authority: which program a session is, how the compositor and sshd come to
   hold a session's place, and which right on a session lets sshd end it).
   init starts a session process per login — the desktop's at boot, one per
   SSH connection at sshd's request — and the compositor and sshd start a
   login's programs under it, never under themselves; sshd ends a
   connection's session when it drops. Its namespace is
   `issues/filesystem/a-user-is-a-home-tree-and-a-login-row.md`'s login row.
   *Exit*: killing the desktop session ends every program the compositor
   started and leaves the compositor running; killing the compositor ends none
   of them; a dropped SSH connection ends its session and all under it.
   Negative control: the stage reverted whole, where the compositor's programs
   die with it. Oracle: systemd-logind, whose session scope ends every process
   of a login.
6. **A program is asked to quit** (Q3b, Q6a–Q6f). `SYS_PROCESS_QUIT` (124,
   never assigned), `(process, reason)`, needs `MANAGE` as the kill does and
   reaches the process's subtree by stage 4's walk; the reason is interrupt,
   hang-up or terminate. It answers 1 while the process's notice still holds a reason
   asked before and not yet read, and 0 otherwise, a process already ended
   included. Each process is started holding its notice, a fourteenth object
   kind under the label `quit`, `READABLE` while a reason is asked and not yet
   read, whose read takes the reasons; a process that has never watched it is
   killed by a quit instead, with its subtree. The kill is unchanged. std's
   `os::toyos` hands a program its notice, as a blocking wait for a reason
   and as the handle for a poller; the `ctrlc` fork `rust/Cargo.toml` patches
   in waits there for the reasons its Unix arm maps, where its ToyOS arm parks
   forever today; rustc stops skipping its handler on ToyOS
   (`rust/compiler/rustc_driver_impl/src/lib.rs`, `install_ctrlc_handler`).
   libc watches the notice from the first `sigaction` that installs a handler
   or `SIG_IGN` for `SIGINT`, `SIGHUP` or `SIGTERM`, and takes interrupt as
   `SIGINT`, hang-up as `SIGHUP` and terminate as `SIGTERM`, by stage 3's
   rule: an ignored reason is dropped, and one with neither handler nor
   ignore ends the process with exit code 128 plus the signal's number.
   `kill` with one of the three is a quit with that reason. *Exit*: quit, a
   child watching its notice writes its file and exits 0, and so does its own
   child, watching too; one that never watched reads killed; one that watches
   and never ends is killed at its parent's deadline; a Rust child whose
   `ctrlc` handler writes its file and exits 0 does so; a C child with LLVM's
   handlers — restore the default, remove its output, raise — that computes
   without calling libc, asked with interrupt, ends at once with its output
   gone and code 130; a C child whose `SIGHUP` handler reloads runs on after a
   hang-up and exits on a terminate; a C child that only ignores `SIGINT`
   runs on after an interrupt; a quit that arrives while `SIGINT` is blocked
   and `ppoll` answers a ready descriptor shows in `sigpending`, and the next
   `ppoll` runs the handler and answers `EINTR`. Negative control: the stage
   reverted whole, where a parent can only kill. Mutations: a quit notifying
   a process that never watched (it runs on); a quit reaching the named
   process alone (the grandchild runs on); `ctrlc` parked as today (killed);
   handlers run only in blocking libc calls (the LLVM child to the hang
   ceiling); a quit without its reason (the reload exits); watching only from
   a handler's install (the ignoring child is killed). Oracle: POSIX's signal
   actions (XSH 2.4.3).
7. **Job control** (Q6a–Q6c, Q7). The shell hands its terminal a
   `MANAGE`-only duplicate of each process of the foreground line over its
   `surface` port. The terminal turns Ctrl+C into their interrupt, and a
   second into another, killing each process whose quit answers that the
   first is still unread; before it closes it asks its shell's tree to hang
   up and kills what is left at its deadline. sshd does that to a dropped
   connection's session, and init at shutdown and test-runner at a test's
   deadline ask with terminate. `&` is a line the shell watches. sshd awaits
   exits through a ToyOS `tokio/src/process` arm in the tokio fork, shaped as
   its Linux pidfd arm (`process/unix/pidfd_reaper.rs`). Stop and continue
   need a suspend primitive and are not proposed. *Exit*: Ctrl+C ends a
   pipeline whose stage started a child, none of it left and the prompt back;
   a child that reads each interrupt survives three Ctrl+C, and one that
   watches and never reads dies on the second; a dropped SSH connection whose
   command started a child leaves neither; tokio's `Child::wait` returns with
   no loop polling `try_wait`. Negative control: today's terminal and shell,
   where the pipeline runs on. Mutation: a kill on any second Ctrl+C (the
   reading child dies). Oracle: POSIX's INTR, a `SIGINT` "sent to all
   processes in the foreground process group" (XBD 11.1.9), and tokio's
   `tests/process_smoke.rs`.
