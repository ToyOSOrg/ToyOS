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
`issues/kernel/toyos-runs-on-arm64.md` need it.

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
- Every process has one parent and any number of children, and a parent's end
  — exit, crash or kill — kills its whole subtree at once; quitting gracefully
  is the parent's job before it exits, never the kernel's. The kernel starts
  init and init starts everything else, with no rule for services: a server
  is init's child and lives as long as init. An app uses a server through
  handles and is never its child. "Keep running after I close this" asks init,
  through `launcher`, to be the parent, and nothing else reparents. A kill is
  cleanup without cooperation, run on the causing path in bounded steps. init
  never dies and stays tiny.

## Stages

1. **init's waiter threads go**. A child's end is readiness on its handle,
   and init still parks one thread per service on `SYS_PROCESS_WAIT`
   (`close_when_it_ends`, `userland/init/src/main.rs`). *Exit*: init starts no
   thread to wait for a service's end, and `close_when_it_ends` is gone with
   all it does kept: a `restart` row that ends is started again on the ports
   it kept; a row without one has its acceptors closed, so a client's next
   connect is `Gone`; a swap's expected end leaves them open for the binary
   after it. `src/metalswap.rs`'s `judge` reads the third, on the T14 through
   `toyos-metal --swap`; no test reads the first two.
2. **An end says how**. An end reads as an exit, a kill or a fault kind
   alike on every architecture, and a bare code reads the last two as failures.
   No end reads as a quit's reason. *Exit*: exited 137, killed, and each fault
   kind the architecture raises, each read from a child that did it. Negative
   control: the stage reverted whole, where the first two read alike. Oracle:
   POSIX `<signal.h>`'s classes.
3. **libc starts and waits for children** (blocked on
   `issues/isolation/a-childs-stdio-handle-is-not-the-one-command-named.md`).
   libc imitates `SIGCHLD`. A C child starts with descriptors 0–2 and exactly
   what its file actions name, a stated departure from POSIX. `posix_spawn` and
   `posix_spawnp` with file actions and attributes, routed by std's
   launch-or-spawn rule moved into `toyos`; a descriptor table with
   close-on-exec; `waitpid`, `wait`, `wait4`; `ppoll`, `sigpending` and the
   signal-set calls; `environ`, `setenv`, `unsetenv`; `kill` with `SIGKILL` for
   a child or a `-pgid` of its children, 0 as a probe, `EINVAL` for other
   signals until stage 6; `system`, `popen` and `pclose` through
   `/system/bin/shell -c`. No `select` in this stage: `issues/build/toyos-builds-itself.md`
   owns it. A handler for any signal libc imitates runs at once, beside the
   program: inside a blocking call of a thread that leaves the signal unblocked
   (`ppoll` and `sigsuspend` under their mask), and, while no thread is in one,
   on libc's own thread. After it `read`, `recv`, `accept`, `wait`, `waitpid`
   and `wait4` restart if it was installed with `SA_RESTART` and answer `EINTR`
   if not, and `poll`, `ppoll`, `pause`, `sigsuspend`, `nanosleep` and `usleep`
   answer `EINTR`, `sleep` the time left; while every thread blocks the signal,
   it stays pending, as `sigpending` reports, until one unblocks it. libc's own
   thread watches at most 255 unwaited children on its own inbox, the 256th
   watch being stage 6's notice, and wakes a thread in a blocking call through
   one pipe, so `poll` takes at most 255 descriptors and children never count
   against them. *Exit*, C corpus files, the mutation that reds an arm after it:
   a piped stdout read to EOF through `poll`, then `waitpid`;
   `waitpid(-1)` answering three children once each in the order they end, then
   `ECHILD` (waiting on the first alone), and `wait` and `wait4` alike (either
   answering `ECHILD` as `waitpid` does today); a killed child `WIFSIGNALED`
   `SIGKILL`, and one per stage 2 fault kind with its `<signal.h>` class's
   signal (every kernel end `SIGKILL`); `setenv` and `unsetenv` seen in a
   child's `environ` (the start-up environment passed); `kill(-pgid, SIGKILL)`
   ending that group's two children and not a third (every child ended), and
   `kill(pid, 0)` answering 0 for a live child and `ESRCH` once it is waited
   (the probe kills); `system` and `pclose` answering a command's exit 3 and
   `popen` its output (the shell's `-c` answering 0); the parent's stdout
   marked `FD_CLOEXEC`, a 0 that `addopen` opened `O_CLOEXEC`, and a 1 that
   `adddup2` made and `addclose` closed, each absent in the child (`F_SETFD`,
   `O_CLOEXEC` or `addclose` ignored); the 256th unwaited child refused
   `EAGAIN` (no capacity check, which panics in the poller), and beside 255 of
   them a `poll` of 255 descriptors answering (children watched in `poll`'s own
   poller, which panics); under `SIG_IGN`, 256 children each ending before the
   next all spawned, then `ECHILD` (ended children kept); a `ppoll` unblocking
   `SIGCHLD` answering `EINTR` once a child ends, and a second, the child
   unwaited, answering its readable pipe, and with nothing ready blocking until
   a second child writes (`SIGCHLD` raised again at every blocking call, or at
   every one that finds nothing ready); a `read` of an empty pipe, `SIGCHLD`
   handled with `SA_RESTART`, answering the byte a second child writes after
   the first ends (`SA_RESTART` ignored); a negative descriptor beside a pipe,
   the pipe answering (passed to the kernel, which ends the caller). Negative
   control: today's libc, against which the corpus does not link. Oracle:
   POSIX's `posix_spawn`, `waitpid`, `poll` and `SA_RESTART`, and signal(7).
5. **A login is a session under init**. Per login init starts a session
   program that logout ends and that only parents what its user starts — the
   desktop's at boot, one per SSH connection at sshd's request. As it starts
   one, init hands the compositor or sshd a right to start programs in that
   session alone. sshd's right also quits and kills it. Its namespace is
   `issues/filesystem/a-user-is-a-home-tree-and-a-login-row.md`'s login row.
   *Exit*: killing the desktop session ends every program the compositor started
   and leaves the compositor running; killing the compositor ends none of them;
   a dropped SSH connection ends its session and all under it. Negative control:
   the stage reverted whole, where the compositor's programs die with it.
   Oracle: systemd-logind, whose session scope ends every process of a login.
6. **A program is asked to quit**. `SYS_PROCESS_QUIT` (124, never
   assigned), `(process, reason)`, needs `MANAGE` as the kill does, and the
   reason is interrupt, hang-up or terminate. A quit reaches the process's
   subtree. Each process is started holding its notice, a new
   object kind under the label `quit`, `READABLE` while a reason is asked and
   not yet read, whose read takes it. A quit kills a process that has never
   watched its notice or whose notice still holds a reason, whoever asks, in
   one act no read comes between. A child started under a process whose notice
   holds a reason starts holding it too, and the quit that put it there asks
   that child no second time. std's `os::toyos` hands a program its notice; the
   `ctrlc` fork `rust/Cargo.toml` patches in waits there for the reasons its
   Unix arm maps; rustc stops skipping its handler on ToyOS
   (`rust/compiler/rustc_driver_impl/src/lib.rs`, `install_ctrlc_handler`).
   Stage 5's session, `/system/bin/terminal` and `/system/bin/shell` listen and
   relay nothing, each ending once what it runs has, `-c` with its status, but
   an interactive shell does not end on an interrupt.
   libc takes the three reasons as `SIGINT`, `SIGHUP` and `SIGTERM`, watching
   the notice once one is handled, ignored or blocked: an ignored reason is
   dropped and an unhandled one ends the process with exit code 128 plus the
   signal's number. `kill` with one of the three is a quit with that reason.
   *Exit*: host — `kernel::proclife`'s interleavings of a notice's read racing a
   second quit kill only where the read comes second; of a spawn racing a quit
   on its spawner leave each child holding the reason once or started after the
   read; of a quit on a process that never watched leave every notice below it
   unreadable until that process is claimed. Guest — quit, a child watching its
   notice writes its file and exits 0, and so does its own child, watching too;
   one that never watched reads killed, and so does a watching child of it, its
   file unwritten; under `shell -c`, a child that reports its watch armed and
   holds its interrupt unread until released survives the quit and ends 130,
   and so does its shell; a desktop session asked to hang up sees a watching
   child under a terminal's shell write its file; a Rust child's `ctrlc`
   handler writes its file and exits 0; a C child with LLVM's handlers —
   restore the default, remove its output, raise — that computes without
   calling libc, asked with interrupt, ends at once with its output gone and
   code 130; a C child's `read` of an empty pipe, `SIGINT` handled without
   `SA_RESTART`, answers `EINTR` on an interrupt; a C child in `pause` whose
   `SIGHUP` handler reloads returns from it after a hang-up and exits on a
   terminate; a C child that only ignores `SIGINT` runs on after an interrupt,
   and so does one spawned ignoring it, while one spawned blocking it, by its
   caller's mask or `SETSIGMASK`, shows it in `sigpending` and ends with 130 on
   unblocking it, and one whose `SETSIGDEF` names it reads killed; a quit that
   arrives while `SIGINT` is blocked and `ppoll` answers a ready descriptor
   shows in `sigpending`, and the next `ppoll` runs the handler and answers
   `EINTR`. Negative control: the stage reverted whole, where a parent can only
   kill. Mutations: the check and the kill in two acts (killed after the read);
   no rule for a spawn under an unread reason (the child runs unasked), and the
   quit asking such a child again (it is killed); the walk depositing the
   reason in each watcher below one that never watched before it claims (a
   notice below readable); a quit notifying a process that never watched (it
   runs on); a quit reaching the named process alone (the grandchild runs on);
   `ctrlc` parked as today (killed); a shell that never listens, and one that
   relays (the held child reads killed); a terminal that never listens (the
   desktop child's file unwritten); handlers run only in blocking libc calls
   (the LLVM child to the hang ceiling); `EINTR` from `poll` alone (`read` and
   `pause` never return); a quit without its reason (the reload exits);
   watching only from a handler's install (the ignoring child is killed); a
   spawn carrying no ignore, no mask or no `SETSIGDEF` (each spawned child in
   turn). Oracle: POSIX's signal actions (XSH 2.4.3) and `posix_spawn`.
7. **Job control**. The shell hands its terminal a `MANAGE`-only
   duplicate of each process of the foreground line over its `surface` port. The
   terminal turns each Ctrl+C into their interrupt, and a second kills only a
   program that has not yet taken the first. Before it closes, the terminal asks
   its shell's tree to hang up and kills what is left at its deadline; sshd does
   that to a dropped connection's session, and init at shutdown and test-runner
   at a test's deadline ask with terminate. `&` is a line the shell watches.
   sshd awaits exits through a ToyOS `tokio/src/process` arm in the tokio fork,
   shaped as its Linux pidfd arm (`process/unix/pidfd_reaper.rs`). *Exit*:
   Ctrl+C ends a pipeline whose stage started a child, none of it left and the
   prompt back, and ends no shell nested on the foreground line; a child that
   reads each interrupt survives three Ctrl+C, each pressed once it reports its
   watch armed or the one before read, and one that reports its watch armed and
   never reads dies on the second; a dropped SSH connection whose session runs a
   watching child sees it write its file before it ends; tokio's `Child::wait`
   returns with no loop polling `try_wait`. Negative control: today's terminal
   and shell, where the pipeline runs on. Mutations: a quit that kills on a
   reason already read (the reading child dies); a session that never listens
   (the file unwritten); a shell that ends on an interrupt (the nested shell).
   Oracle: POSIX's INTR, a `SIGINT` "sent to all processes in the foreground
   process group" (XBD 11.1.9), and tokio's `tests/process_smoke.rs`.
