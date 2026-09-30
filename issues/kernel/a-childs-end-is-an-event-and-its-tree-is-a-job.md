---
status: open
kind: track
opened: 2026-09-29
---

# A child's end is an event on its handle, and its tree is a job

libc cannot start a child: `fork` and `execvp` answer `ENOSYS`, `waitpid`
`ECHILD` and `system` `-1` (`userland/libc/src/misc.rs`,
`userland/libc/src/stdio.rs`), and there is no `posix_spawn`. M2 and M4 of
`issues/build/toyos-builds-itself.md` and the exit of
`issues/kernel/toyos-runs-on-arm64.md` need it: a compiler driver runs its
linker, cargo runs rustc, CMake and Ninja run compilers. Stages 2 to 6 are
blocked on
`issues/kernel/the-child-process-track-waits-on-the-owners-rulings.md`.

What stays: a spawn answers a `Process` handle and is the child's whole
authority — the child holds what the slot map duplicates and the endowment
moves; its exit lives on the object, so no parent reaps it and no orphan is
adopted; a pid is a name the kernel never reissues (`kernel/src/id_map.rs`).
What is missing:

- **An end is not an event.** `OP_WATCH` on a `Process` answers `NotSupported`
  (`kernel/src/object/ops.rs`, `read_watch`), so init parks a thread per serving
  service in `SYS_PROCESS_WAIT` (`userland/init/src/main.rs`,
  `close_when_it_ends`), sshd polls `try_wait` on a 1–25 ms backoff
  (`userland/sshd/src/main.rs`, `reap`), and a `waitpid(-1)` has nothing to
  wait on.
- **An end does not say how.** A kill publishes 137, a handle fault 139 and a
  CPU fault -1 (`kernel/src/process.rs`, both architectures' trap paths), codes
  a program can exit with too, and std's `ExitStatus::code()` is never `None`.
- **A kill ends one process**, and there is no group to kill.
- **Nothing asks a program to quit.** A kill is the only end another program
  can cause, and neither `userland/terminal` nor `userland/shell` does anything
  with Ctrl+C.

**Constraints.** libc is built on ToyOS and never ToyOS on libc (owner,
2026-09-30: "toyos moderness and anti legacy may never be compromised because
of libc. no legacy"): nothing in the kernel or the `toyos` crate exists for
libc's sake, and libc builds Unix's surface — `SIGCHLD`, `SIGINT` and `SIGTERM`
handlers, `waitpid`, `posix_spawn` — out of ToyOS events alone. There is one
mechanism (owner, 2026-09-30: "i dont want multiple mechanisms"): the `Process`
handle is the only control, and the kernel grows no job object, process group,
session, controlling terminal, signal, wait-any, pid-addressed call or thread.
Each new cost is paid by the thread that causes it. What names a child's image
and working directory is the storage and isolation tracks'
(`issues/kernel/the-kernel-is-small-interrupts-post-and-threads-wait.md` step
5, `issues/isolation/every-program-sees-only-the-files-it-was-given.md` stage
1).

**Ruled** (owner, 2026-09-30):

- A child's end is readiness on its handle: `OP_WATCH` `READABLE` on a
  `Process` (`WAIT`) completes once the exit is published, closing one handle
  ends no other's watch, nothing polls, and the ABI gains nothing.
- `SYS_PROCESS_OPEN` (110) is retired, as stage 0, landable on its own.

## Stages

0. **`SYS_PROCESS_OPEN` goes** (ruled). Its only caller is its test
   (`tests/toyos-rust-tests/src/bin/process_lifecycle.rs`), so nothing replaces
   it. Deleted: `sys_process_open` and its dispatch arm, 110 entering
   `retired_syscalls!`; `toyos_abi::syscall::process_open` and
   `SysCap::open_process`; `MANAGE` in init's `SysCap`
   (`kernel/src/loader/mod.rs`, `spawn_init`) and its meaning on one; the
   `reopenable` column (`kernel/src/object/mod.rs`'s `kobject!`,
   `kernel/src/object/handle.rs`) and `process::process_object`;
   `reopen_selftest` (`kernel/src/object/process.rs`),
   `sched::kthread::open_selftest`, the `process-reopen-selftest` actuator
   (`kernel/src/actuator.rs`) and its two call sites (`kernel/src/main.rs`);
   the `process_reopen_selftest` guest test with its judge and its rows in
   `tests/toyos.rs`, `src/metal.rs` and `tests/test-durations`; and
   `process_lifecycle`'s arm that opens a process by pid. *Exit*: a call of 110
   answers `NotSupported` and the log names it retired, and none of the deleted
   names is left in the tree.
1. **An end is an event** (ruled). The kernel answers `read_watch` and
   `has_data` for a `Process`, whose watch becomes an `Arc` as an `Acceptor`'s
   is, and `close_ends_polls` answers `false` for one: a holder's close ends no
   other holder's watch. init's waiter threads go. *Exit*: a guest test holds
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
   `ProcessObject` carry exited, killed or faulted; std's `ExitStatus`, a ToyOS
   `ExitStatusExt` that reads what ended a process, and the SDK decode it.
   *Exit*: a guest test reads exited 137 from a child that exits 137, killed
   from one killed, and each fault kind its architecture raises from a child
   that commits it. Negative control: the stage reverted whole, whose code
   alone cannot tell the first two apart. Oracle: POSIX `<signal.h>`'s
   classes.
3. **libc starts and waits for children** (Q3; blocked on
   `issues/isolation/a-childs-stdio-handle-is-not-the-one-command-named.md`,
   whose undeclared child is `posix_spawn`'s shape). std's launch-or-spawn rule
   (`rust/library/std/src/sys/process/toyos.rs`, `Command::spawn`) moves into
   `toyos`, beside the launcher protocol it drives (`toyos/src/launch.rs`), and
   std and libc both call it. libc gains a descriptor table — each
   descriptor's handle and close-on-exec flag, which `open` sets for
   `O_CLOEXEC` and `fcntl` reads and writes for `F_GETFD` and `F_SETFD` — and
   `posix_spawn` and `posix_spawnp` with their file actions (`addopen`,
   `adddup2`, `addclose`, `addchdir`) and attributes (`setflags`,
   `setpgroup`, `setsigmask`), passing what Q3's rule names through the slot
   map. `waitpid`, `wait` and `wait4` (LLVM's `Support/Unix/Program.inc` waits
   with it; `rusage` from `SYS_PROCESS_STATS`) read libc's pid → handle map;
   `waitpid(-1)` watches every unwaited child on one inbox kept for the
   process, and a child past its capacity (`Poller::MAX_HANDLES`, 256) is
   refused `EAGAIN`. libc delivers `SIGCHLD` as Q3 rules and gains `ppoll` in
   `poll.h`, which Ninja's own build then selects (`CMakeLists.txt`,
   `HAVE_PPOLL`), `sigpending`, `sigismember` and the rest of the signal-set
   calls, an exported `environ`, and a `setenv` and `unsetenv` that change it.
   It keeps each process group as the set of its own children in it. `kill`
   with `SIGKILL` ends a child, or each child of a group for `-pgid`; with 0 it
   answers `0` for a child not yet waited for and `ESRCH` for any other pid;
   it refuses every other signal `EINVAL` until stage 5. `system`, `popen` and
   `pclose` spawn `/system/bin/shell -c`. `fork` stays `ENOSYS`. A
   descriptor's number is its handle, so it can pass `FD_SETSIZE` and an
   inherited one is renumbered: `select` and `pselect` are not offered. Owed
   elsewhere: `/bin/sh`
   (`issues/build/ninja-runs-every-command-through-a-bin-sh-toyos-does-not-have.md`),
   `/dev/null` (`issues/filesystem/there-is-no-dev-null.md`), and
   `dlsym(RTLD_DEFAULT, …)`, which libuv's first spawn calls to find
   `posix_spawn_file_actions_addchdir`
   (`issues/build/libc-dlsym-rtld-default-panics-as-unimplemented.md`).
   *Exit*: C corpus files — a child with a piped stdout read to EOF through
   `poll`, then `waitpid`ed with its status; three children `waitpid(-1)`
   answers once each in the order they end, then `ECHILD`; a killed child
   `WIFSIGNALED` with `SIGKILL`; a descriptor opened `O_CLOEXEC`, one marked
   `FD_CLOEXEC` and one closed by `posix_spawn_file_actions_addclose` each
   absent in the child; the 257th unwaited child refused `EAGAIN`; with
   `SIGCHLD` at `SIG_IGN`, 257 children that each end before the next starts
   all spawned, and `waitpid(-1)` then `ECHILD`; a `ppoll` whose mask unblocks
   `SIGCHLD` answering `EINTR` once a child ends, and a second `ppoll`, with
   that child still unwaited and its pipe readable, answering the pipe.
   Mutations, each red on its arm: a `waitpid(-1)` that waits on its first
   child alone (the order); a spawn that passes every handle (the absent
   descriptors); no capacity check (`EAGAIN`, which instead panics in the
   poller's registration assert); an ignored `SIGCHLD` that keeps ended
   children (`SIG_IGN`); `SIGCHLD` raised at every blocking call while an ended
   child is unwaited (the second `ppoll`). Oracle: POSIX's `posix_spawn`,
   `waitpid` and `ppoll`.
4. **A program owns what it starts** (Q4). A spawned child runs under its
   spawner, a launched one under the process that asked for it, and a
   `service = true` program under init however it is started. The kernel
   keeps, on each process's table entry, the pid of the process it runs under
   and the pids of those under it: bookkeeping that only the kill and the end
   walk read, which no syscall takes or answers, so a number is never
   authority. Killing a process ends it and everything under it and admits
   nothing under it after; so does its own end, however it comes. The walk is
   paid by the killing or ending thread: it closes admission at the top, then
   claims one process at a time, taking the table lock for each and none
   across the tree, so it is bounded by the processes in the tree. A process's
   end is published once its own teardown is done and everything under it has
   published — a count on each process that each child's publication lowers
   once — so each end is one step, and the steps one end sets off are as many
   as the tree is deep. Each process is started holding a handle to itself
   carrying only `WRITE`, to start a child under it, with `DUP` and
   `TRANSFER`; std and libc send a copy with every launch in its handle batch,
   which three slots and five extras fill to eight today
   (`toyos/src/launch.rs`), so the extras drop to four. init spawns the child
   under it, `SpawnArgs` gaining the place (96 bytes to 104), and refuses a
   launch that carries none: nothing starts, so a program holding `launcher` —
   the shell, and every process that inherits its namespace — starts nothing
   outside its own tree. The kill keeps its number, name and arguments (109,
   `SYS_PROCESS_KILL`). Placement, admission, the kill and publication are
   decided in `toyos-proclife`, and init's part lands in whichever crate holds
   launch resolution by then
   (`issues/isolation/the-supervisor-is-host-tested-and-owns-the-stop.md` stage
   2). *Exit*: host — `toyos-proclife`'s interleavings of a spawn racing the
   kill of the process it lands under hold that nothing lands under a killed
   process and that everything under it when the kill is claimed ends, and red
   under a mutation that admits under a killed process. Guest — A starts B, B
   starts C and launches D: killing B ends B, C and D, each read as killed, and
   A's watch on B completes only after all three have ended; B ending on its
   own ends C and D the same way; a chain of 64 processes, each started by the
   one before, ends whole when its first is killed, and the first's end is
   published after the last's; a spawn or launch under B after its kill is
   refused `Gone`; a launch that carries no place is refused and nothing
   starts; a `MANAGE`-only duplicate given as a place answers
   `PermissionDenied`. Negative control: the stage reverted whole, where
   killing B, all A can name, leaves C and D running. Mutations: a kill that
   skips a launched child leaves D; an end published before what is under it
   reds A's watch. Oracle: cgroup v2's `cgroup.kill`, which ends everything in
   a cgroup and below it and admits no fork after.
5. **A program is asked to quit** (Q6). A quit on a `Process` handle makes
   the notice that process was started holding `READABLE`, an event it waits
   on beside any other; a process that has never watched its notice is killed
   instead. The SDK hands a program its notice to register in its poller; libc
   runs the first of `SIGINT`, `SIGTERM` and `SIGHUP` the program handles when
   a quit arrives inside a blocking libc call, as it does `SIGCHLD`, and `kill`
   with any of the three is a quit. *Exit*: a guest test's child that watches
   its notice, quit, writes its file and exits 0; one that never watched it,
   quit, reads killed; one that watches and does not end is killed by its
   parent once the parent's deadline on its end has passed; a C child with a
   `SIGTERM` handler runs it on `kill(pid, SIGTERM)`. Negative control: the
   stage reverted whole, where the only end a parent can cause is a kill.
   Mutation: a quit that notifies a process that never watched its notice
   leaves it running. Oracle: POSIX's signal actions (XSH 2.4.3), where a
   default action ends the process and an installed handler runs instead.
6. **Job control.** The shell hands its terminal a `MANAGE`-only duplicate of
   each process it started for the line in the foreground, over the terminal's
   `surface` port, whose connector the terminal already provides it; the
   terminal's translator turns Ctrl+C into their quit — a tty's `SIGINT` to the
   foreground group, with no tty in the kernel — and a second Ctrl+C, or their
   ends not come by the terminal's deadline, into their kill. A closed window,
   a dropped SSH session, shutdown and a test past its deadline are the same
   pair, from the terminal, sshd, init and test-runner. `&` is a line the
   shell watches. sshd awaits exits through the tokio fork, which gains a
   `tokio/src/process` arm for ToyOS — neither `unix` nor `windows` there, so
   the fork at `d6c5691c` has none — registering the handle as its Linux arm
   registers a pidfd (`process/unix/pidfd_reaper.rs`); sshd then builds tokio
   with `process`, which `userland/sshd/Cargo.toml` leaves out. Stop and
   continue need a suspend primitive and are not proposed. *Exit*: a pipeline
   whose stage started a child is ended by Ctrl+C with none of it left and the
   prompt back, and a stage that ignores the quit by a second Ctrl+C; a dropped
   sshd session whose command started a child leaves neither; tokio's
   `Child::wait` returns with no loop polling `try_wait`. Negative control:
   today's terminal and shell, which handle Ctrl+C nowhere, where the pipeline
   runs on. Oracle: POSIX's INTR, a `SIGINT` "sent to all processes in the
   foreground process group" (XBD 11.1.9), and tokio's own
   `tests/process_smoke.rs`, which runs `sh -c`.
