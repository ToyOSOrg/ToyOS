---
status: open
kind: track
opened: 2026-09-29
---

# A child's end is an event, and a parent takes its children down

A child starts by spawn, never by fork: `fork` and the `exec` family answer
`ENOSYS`, and libc starts one by `posix_spawn` (stage 3). What is left is a
child's end as a signal (stage 4), sessions (5), a request to quit (6) and job
control (7). M4 of `issues/toyos-builds-itself.md` and the exit of
`issues/toyos-runs-on-arm64.md` need the rest.

**Constraints.** libc is built on ToyOS and never ToyOS on libc (owner,
2026-09-30: "toyos moderness and anti legacy may never be compromised because
of libc. no legacy"). One mechanism (owner: "i dont want multiple
mechanisms"): the `Process` handle is the only control, and the kernel grows
no job object, process group, POSIX session, controlling terminal, signal,
wait-any, pid-addressed call or thread. What names a child's image and working
directory is `issues/every-program-sees-only-the-files-it-was-given.md`'s.

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

1. **Done** (#926, stage 1.2 of
   `issues/a-device-function-is-driven-from-its-arrival-to-its-end.md`):
   **init's waiter threads go**; the supervisor watches each child's end on
   its own loop's poller. *Exit*: init starts no
   thread to wait for a service's end, and `close_when_it_ends` is gone with
   all it does kept: a `restart` row that ends is started again on the ports
   it kept; a row without one has its acceptors closed, so a client's next
   connect is `Gone`; a swap's expected end leaves them open for the binary
   after it. No test reads any of the three: `src/metalswap.rs`'s `judge`
   read the third, on the T14 through `toyos-metal --swap`, and is deleted
   (`issues/the-host-cannot-reach-the-t14-while-it-runs-toyos.md`).
2. **Done**: **an end says how**. `SYS_PROCESS_WAIT` answers a
   `toyos_abi::end::End` — an exit with its code, a kill, or a fault of one of
   `<signal.h>`'s classes (memory, alignment, instruction, arithmetic, trap)
   or the handle fault — classified at each architecture's fault path, and
   std's `ExitStatus` has no code for the last two and reads them as failures.
   No end reads as a quit's reason. Read from children that did each:
   `process_lifecycle` (exited 137, killed), `fault_gates` and `claim_bar_fault`
   (memory, instruction, arithmetic), `debug_trap` (trap), the handle-fault
   probes (handle). x86-64 raises no alignment fault (`CR0.AM` is clear), and
   AArch64's `virt` rows read only memory and instruction until
   `issues/toyos-runs-on-arm64.md`'s stage 8 boots the shared suite there.
   Oracle: POSIX `<signal.h>`'s classes.
3. **Done**: **libc starts and waits for children**. `posix_spawn` and
   `posix_spawnp` with file actions and attributes start a child by
   `toyos::spawn`, std's launch-or-spawn rule, which std's `Command` now uses
   too. A C child starts with descriptors 0–2 and exactly what its file
   actions name, a stated departure from POSIX, and a spawned child's table
   keeps slots 0–2 for them at its spawn (`HandleTable::for_child`): an
   endowment that filled an empty one read as the child's stdio. A stdio
   slot a process was started without is no descriptor of its children
   (`toyos::spawn::inherited`), and std's stdio answers its errors rather
   than reading every one as a closed stream (`is_ebadf`). libc
   keeps close-on-exec marks and what slots 0–2 hold (`fdreq`); `waitpid`,
   `wait` and `wait4` answer each child once, by pid, group or any, with
   `<sys/wait.h>`'s status for its stage 2 end (`spawnreq`); `environ`,
   `setenv` and `unsetenv`; `kill` with `SIGKILL` for a child or a group of
   them, 0 as a probe, `EPERM` for its own pid (a process holds no right to
   kill itself) and `EINVAL` for other signals until stage 6; `system`,
   `popen` and `pclose` through `/system/bin/shell -c`; at most 255
   unwaited children, the 256th refused `EAGAIN`. *Exit*, met by
   `tests/guest/libc` on QEMU x86-64 (`libc_children`), each row with the
   mutation that reds it: a piped stdout read to EOF through `poll`, then
   `waitpid`; a child's write refused `EPIPE` once its pipe's one reader
   closed; a child given a pipe's write end as stdin refused its read (the
   `adddup2` onto 0 ignored); `waitpid(-1)` answering three children once
   each in the order they end, then `ECHILD` (waiting on the first alone),
   and `wait` and `wait4` alike (`wait` answering `ECHILD`); a killed child
   `WIFSIGNALED` `SIGKILL`, and one per stage 2 fault kind with its
   `<signal.h>` class's signal (every kernel end `SIGKILL`); `setenv` and
   `unsetenv` seen in a child's `environ` (the start-up environment passed);
   `kill(-pgid, SIGKILL)` ending that group's two children and not a third
   (every child ended), and `kill(pid, 0)` answering 0 for a live child and
   `ESRCH` once it is waited (the probe kills); `system` and `pclose`
   answering a command's exit 3 and `popen` its output (`system` answering
   0); the parent's stdout marked `FD_CLOEXEC`, a 0 that `addopen` opened
   `O_CLOEXEC`, and a descriptor that `adddup2` made and `addclose` closed,
   each absent in the child (`F_SETFD`, `O_CLOEXEC` or `addclose` ignored,
   and the child's table without its stdio slots kept); the 256th unwaited
   child refused `EAGAIN` beside a `poll` of 255 descriptors answering (no
   capacity check); a negative descriptor beside a pipe, the pipe answering
   (passed to the kernel, which ends the caller).
4. **libc imitates `SIGCHLD`**. A handler for any signal libc imitates runs
   at once, beside the program: inside a blocking call of a thread that leaves
   the signal unblocked (`ppoll` and `sigsuspend` under their mask), and,
   while no thread is in one, on libc's own thread. After it `read`, `recv`,
   `accept`, `wait`, `waitpid` and `wait4` restart if it was installed with
   `SA_RESTART` and answer `EINTR` if not, and `poll`, `ppoll`, `pause`,
   `sigsuspend`, `nanosleep` and `usleep` answer `EINTR`, `sleep` the time
   left; while every thread blocks the signal, it stays pending, as
   `sigpending` reports, until one unblocks it. libc's own thread watches the
   unwaited children, at most 255 as stage 3 bounds them, the 256th watch
   being stage 6's notice, on its own inbox, and wakes a thread in a blocking
   call through one pipe, so `poll` takes at most 255 descriptors and children
   never count against them; stage 3's `waitpid(-1)` watches the children
   unwaited when it was called, so one another thread starts after it, ending
   first, does not wake it, which libc's own thread ends. `ppoll`,
   `sigpending` and `sigsuspend` come with it. *Exit*, C corpus files beside stage 3's, the mutation that reds an arm
   after it: under `SIG_IGN`, 256 children each ending before the next all
   spawned, then `ECHILD` (ended children kept); a `ppoll` unblocking
   `SIGCHLD` answering `EINTR` once a child ends, and a second, the child
   unwaited, answering its readable pipe, and with nothing ready blocking until
   a second child writes (`SIGCHLD` raised again at every blocking call, or at
   every one that finds nothing ready); a `read` of an empty pipe, `SIGCHLD`
   handled with `SA_RESTART`, answering the byte a second child writes after
   the first ends (`SA_RESTART` ignored). Oracle: POSIX's `waitpid`, `poll`
   and `SA_RESTART`, and signal(7).
5. **A login is a session under init**. Per login init starts a session
   program that logout ends and that only parents what its user starts — the
   desktop's at boot, one per SSH connection at sshd's request. As it starts
   one, init hands the compositor or sshd a right to start programs in that
   session alone. sshd's right also quits and kills it. Its namespace is
   `issues/a-user-is-a-home-tree-and-a-login-row.md`'s login row.
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
