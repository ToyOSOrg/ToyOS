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
no job-control object, process group, POSIX session, controlling terminal,
signal, wait-any, pid-addressed call or thread. The scheduler's job (#979,
`kernel::sched::job`) is the owner CPU time is shared under, and no quit or
kill reads it. What names a child's image and working directory is
`issues/every-program-sees-only-the-files-it-was-given.md`'s.

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

  Since stage 5, init is the supervisor and, for each login session, its
  owner: "keep running after I close this" in a login session places the app
  under that session's owner, and only in the machine's session under the
  supervisor.

**Ruled** (owner, 2026-10-21): libc's signals are the focused set this track
plans, over native process events: a child's end (stage 4) and an interrupt,
hang-up or terminate request (stage 6), and no POSIX signal subsystem up
front. libc's adaptation stays in userland, and ordinary Rust behaviour is
kept. Further signal behaviour comes when a required program shows a
concrete need, with its semantics and cost reviewed then. An unsupported
signal operation never appears to succeed.

`signal` and `sigaction` keep a handler only for a signal an event raises
(`SIGCHLD`, `SIGALRM`, `SIGINT`, `SIGHUP`, `SIGTERM`) and refuse one for any
other, which nothing would call (`sdk/libc/src/sigreq.rs`).

init is `/system/bin/supervisor`.

**Ruled** (owner, 2026-10-22): one userland session-owner process per login
session, which also provides that session's launcher, with no separate session
or launcher daemon. The supervisor owns the session owners, and each session
owner owns its desktop or terminal tree and the apps detached from it. An app
meant to outlive the shell that started it starts under the session owner, not
the supervisor: closing the shell leaves it running, and switching users keeps
the session alive. Ordinary shell commands spawn helpers, which inherit only
what is passed to them; `launch <app>` is the explicit independent launch, with
the app's own identity and saved decisions, and independent identity does not
mean detached lifetime. Sign-out coordinates a graceful shutdown, with cancel
and force, before it ends the session's subtree. A session owner's failure may end
that session, never another session or a system service. SSH
authentication starts the user's session, and its shell inherits no host key
and no SSH service authority. The kernel's mechanisms stay ordinary processes,
parent and child, jobs and capabilities; login and session policy is
userland's.

**No timer ends a process.** A dropped session or a closed terminal sends its
tree a hang-up quit, says what stays, by name and how long it has been
outstanding, and keeps waiting. Only an explicit force ends the rest: a
sign-out's force, through a capability the supervisor mints scoped to the
user's own sessions, or the controls' stop. The kernel's second quit, which
kills a process that has not read the first, is such a force when a person or
an explicit force issues it, and never a timer's.

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
   `toyos::spawn`, as a helper, as std's `Command` does by default. A C child starts with descriptors 0–2 and exactly what its file
   actions name, a stated departure from POSIX, each endowed as
   `fd:<n>`, which the child's descriptor table installs at its number
   (`toyos::fd`). A descriptor a process was started without is no
   descriptor of its children (`toyos::fd::copy`), and std's stdio answers
   its errors rather than reading every one as a closed stream
   (`is_ebadf`). libc's close-on-exec flag is the table's per-descriptor
   inheritable flag; `waitpid`,
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
   and the child's stdio not taken from its endowments); the 256th unwaited
   child refused `EAGAIN` beside a `poll` of 255 descriptors answering (no
   capacity check); a negative descriptor beside a pipe, the pipe answering
   (passed to the kernel, which ends the caller).
4. **Done** (#1011): **libc imitates `SIGCHLD`**. A handler for any signal libc
   imitates runs at once, beside the program: inside a blocking call of a
   thread that leaves the signal unblocked (`ppoll` and `sigsuspend` under
   their mask), and, while no thread is in one, on libc's own thread. After it
   `read`, `recv`, `accept`, `wait`, `waitpid` and `wait4` restart if it was
   installed with `SA_RESTART` and answer `EINTR` if not, and `poll`, `ppoll`,
   `pause`, `sigsuspend`, `nanosleep` and `usleep` answer `EINTR`, `sleep` the
   time left; while every thread blocks the signal, it stays pending, as
   `sigpending` reports, until one unblocks it. libc's own thread watches the
   unwaited children, at most 255 as stage 3 bounds them, the 256th watch being
   stage 6's notice, on its own inbox, and wakes a thread in a blocking call
   through one pipe, so `poll` takes at most 255 descriptors and children never
   count against them; stage 3's `waitpid(-1)` watches the children unwaited
   when it was called, so one another thread starts after it, ending first,
   does not wake it, which libc's own thread ends. `ppoll`, `sigpending` and
   `sigsuspend` come with it. *Exit*, C corpus files beside stage 3's, the
   mutation that reds an arm after it: under `SIG_IGN`, 256 children each
   ending before the next all spawned, then `ECHILD` (ended children kept); a
   `ppoll` unblocking `SIGCHLD` answering `EINTR` once a child ends, and a
   second, the child unwaited, answering its readable pipe, and with nothing
   ready blocking until a second child writes (`SIGCHLD` raised again at every
   blocking call, or at every one that finds nothing ready); a `read` of an
   empty pipe, `SIGCHLD` handled with `SA_RESTART`, answering the byte a second
   child writes after the first ends (`SA_RESTART` ignored). Oracle: POSIX's
   `waitpid`, `poll` and `SA_RESTART`, and signal(7). *Met* in
   `libc_children`'s QEMU x86-64 boot by `signal_sigchld_ignored`,
   `signal_sigchld_ppoll` and `libc_read_restarts`, each red under its mutation
   and the stage reverted whole, beside `signal_alarm`; the routing laws are
   host tests (`signal_dispositions.rs`), and so is a wait deciding from one
   read of each child's end (`spawn_requests.rs`). libc's own thread also watches its
   control event, so it watches at most 254 children: the 255th is refused
   `EAGAIN`, a `poll` takes 255 descriptors beside its wake event, and `SIG_IGN`
   is shown over 255 children.
5. **A login is a session under its owner**. **In review** (#1035, the P1b pull
   request). Per login the supervisor starts one session owner,
   `/system/bin/session`, which accepts on the session's launcher and hands each
   connection to the supervisor with its badge; the supervisor serves the
   launch in the session of the owner's link, never the request's. It parents
   the session's detached apps, which the supervisor spawns under the place
   the owner gives it, and the supervisor tells it of each app's start and
   end. The desktop's session opens with the screen's row, endowed to it, and
   again when it ends; any other `login` row opens one on the supervisor's
   `login` port, sshserver one per connection. A `login` row holds no
   launcher of the machine's session: its launches are its sessions' alone,
   and with none open it launches nothing and says so. `launch <app>` asks the shell's
   session for an independent app; a bare command is the shell's helper.
   Sign-out asks every app to quit with a hang-up, reports what stays to the
   session's controls, keeps the session on a cancel and ends what stays only
   on a force; `logout`, the one row holding `session-admin`, lists the
   sessions open and signs one out. The machine has one user, so a login
   session's `logout` acts on every session. An owner's end other than its
   sign-out's is a failure, and a login authority whose sessions failed more
   than the restart budget's three times in ten seconds is refused its next
   open; no other open is counted. Its namespace is
   `issues/a-user-is-a-home-tree-and-a-login-row.md`'s login row; the SSH
   session's shell holds no host key and no SSH service authority.
   *Exit*: ending the desktop's session owner ends every program of that
   session and leaves the compositor and every other session running; closing
   a shell leaves an app it launched running under the session owner; a
   dropped SSH connection hangs up its session's tree, says what stays and ends
   it only on a force; the SSH session's shell can name no host key. Oracle:
   systemd-logind, whose session scope ends every process of a login.
   - Met in `session_lifecycle`'s QEMU x86-64 boot (`tests/images/sessioncase.toml`):
     a detached app outlives its shell; the desktop's sign-out, by its keys,
     waits for an app that ignores a hang-up, Escape keeps it and Enter kills
     it, and the compositor opens the desktop again; a job's session, opened
     on `login`, signs out the same way, `logout` lists it active and ending,
     and ten sessions open and sign out in a row. Jobs of a `login`
     test-runner run in a session it opens for them.
   - The owner's unasked end ending its apps has no guest arm, since nothing
     may kill a process by its number: the kernel's parent rule is
     `process_tree`'s, and the supervisor's half, placing detached apps under
     the owner and reading its unasked end as the session's failure, is
     host-tested (`a_detached_app_is_placed_under_its_sessions_owner`,
     `an_owner_that_ends_unasked_ends_its_session_as_a_failure`).
   - The SSH arms wait for a T14 row of sshserver, which this stage owes:
     until then a connection's session and its sign-out are only built.
   - Negative control: the stage reverted builds neither the job nor its
     image, so the controls are mutations, each red by name: a graceful
     sign-out that kills (`session_lifecycle` stalls at Escape's cancel, and
     the owner's host laws red); a handed-over launch in its badge's session;
     a sibling's sign-out forgetting a failure; one failure refusing a login;
     a detached app placed under the supervisor; a `login` row holding the
     machine's launcher; any row receiving `session-admin`.
6. **A program is asked to quit**.
   - 6.1 **Done** (#995, the kernel's half): `SYS_PROCESS_QUIT` (135: the 124 this
     stage named went to `SYS_COUNTERS`), `(process, reason)`, needs `MANAGE`
     as the kill does, and the reason is `toyos_abi::quit::Reason`:
     interrupt, hang-up or terminate. A quit reaches the process's subtree.
     Each process is started holding its notice, the object kind `Notice`
     under `QUIT_LABEL` (`quit`), `READ` and `WAIT` alone, readable while a
     reason is asked and not yet read, whose read takes it and an empty read
     takes nothing. A process listens from its first read of its notice or
     the first readable watch armed on it. A quit kills a process that never
     listened or whose notice still holds a reason, whoever asks, in one act
     no read comes between, and asks nothing below it. A child started under a
     process whose notice holds a reason starts holding it too, and the quit
     that put it there asks that child no second time (`toyos_proclife::quit`).
     *Exit*, met: host — `proclife::interleave`'s orderings of a notice's
     read racing a second quit kill only where the read comes second, of a
     spawn racing a quit on its place ask each child once, of a quit of a
     process that never listened ask nothing below it, and of a quit of a
     listening one reach everything below it, each with its `mutate-quit-*`
     control; guest, `quit_tree` in `libc_children`'s QEMU x86-64 boot —
     two generations listening by read and by watch each read the reason and
     exit 0, one that never listened is killed and its listening child with
     it unread, a second quit kills a reason left unread, a reader survives
     three, and a quit without `MANAGE` is refused. Negative control: a
     readable watch that listens to nothing reds the watch arm.
   - 6.2 **In progress** (#1011 lands libc's arms). std's `os::toyos` hands a
     program its notice; the `ctrlc` fork `rust/Cargo.toml` patches in waits
     there for the reasons its Unix arm maps; rustc stops skipping its handler
     on ToyOS (`rust/compiler/rustc_driver_impl/src/lib.rs`,
     `install_ctrlc_handler`). Stage 5's session, `/system/bin/terminal` and
     `/system/bin/shell` listen and relay nothing, each ending once what it
     runs has, `-c` with its status, but an interactive shell does not end on
     an interrupt. libc takes the three reasons as `SIGINT`, `SIGHUP` and
     `SIGTERM`, watching the notice once one is handled, ignored or blocked: an
     ignored reason is dropped and an unhandled one ends the process with exit
     code 128 plus the signal's number. `kill` with one of the three is a quit
     with that reason. *Exit*, guest: under `shell -c`, a child that reports
     its watch armed and holds its interrupt unread until released survives the
     quit and ends 130, and so does its shell; a desktop session asked to hang
     up sees a watching child under a terminal's shell write its file; a Rust
     child's `ctrlc` handler writes its file and exits 0; a C child with LLVM's
     handlers — restore the default, remove its output, raise — that computes
     without calling libc, asked with interrupt, ends at once with its output
     gone and code 130; a C child's `read` of an empty pipe, `SIGINT` handled
     without `SA_RESTART`, answers `EINTR` on an interrupt; a C child in
     `pause` whose `SIGHUP` handler reloads returns from it after a hang-up and
     exits on a terminate; a C child that only ignores `SIGINT` runs on after
     an interrupt, and so does one spawned ignoring it, while one spawned
     blocking it, by its caller's mask or `SETSIGMASK`, shows it in
     `sigpending` and ends with 130 on unblocking it, and one whose `SETSIGDEF`
     names it reads killed; a quit that arrives while `SIGINT` is blocked and
     `ppoll` answers a ready descriptor shows in `sigpending`, and the next
     `ppoll` runs the handler and answers `EINTR`. Negative control: the stage
     reverted whole, where a parent can only kill. Mutations: `ctrlc` parked as
     today (killed); a shell that never listens, and one that relays (the held
     child reads killed); a terminal that never listens (the desktop child's
     file unwritten); handlers run only in blocking libc calls (the LLVM child
     to the hang ceiling); `EINTR` from `poll` alone (`read` and `pause` never
     return); a quit without its reason (the reload exits); watching only from
     a handler's install (the ignoring child is killed); a spawn carrying no
     ignore, no mask or no `SETSIGDEF` (each spawned child in turn). Oracle:
     POSIX's signal actions (XSH 2.4.3) and `posix_spawn`. *Met by #1011* in
     `libc_children`'s QEMU x86-64 boot (`signal_quit`): the LLVM child, the
     `read` answering `EINTR`, the reload in `pause` and its terminate, the
     child that only ignores `SIGINT`, and the quit held over a ready `ppoll`;
     with the stage's libc reverted whole the cases do not build
     (`sig_atomic_t` is undeclared), so that control runs no case, and the arm
     controls are the mutations: handlers run only in blocking calls, `EINTR`
     from `poll` alone, a quit without its reason and watching only from a
     handler's install each red its arm. *Open*: the spawned children's ignore, mask and `SETSIGDEF`, whose
     carrying across a spawn is a decision with the owner, and with it their
     spawn mutations; and the shell, terminal, session and `ctrlc` arms, stage
     7's. Until a spawn carries them a child starts with every signal at its
     default and none blocked, and a `SETSIGMASK` naming a signal is refused
     `ENOSYS`.
7. **Job control**.
   - 7.1 **Done** (#1017): an interactive shell hands its surface, the
     terminal's or the console's, a duplicate carrying `MANAGE` of each process
     of the line it waits for (`toyos::surface::Foreground`), and a `-c` shell,
     itself a process of the line that started it, hands nothing. The surface
     turns each Ctrl+C into an interrupt of the line set most recently
     (`Host::interrupt`), so a shell nested on another's line is asked only at
     its prompt, and passes the key on as its byte when no line is held. The
     shell and the terminal listen: an interactive shell does not end on an
     interrupt, a `-c` shell ends once what it runs has, with its status, and a
     hang-up or a terminate ends either once its line has. Closing a terminal's
     window asks the shell's tree to hang up; a hang-up or terminate of the
     terminal reached that tree already and asks nothing more
     (`surface::Leaving`). Either way the terminal waits, saying every ten
     seconds what it still waits for. That close path is host-tested as a
     decision only, and unverified until 6.2's desktop hang-up arm is met.
     One copy of `toyos` takes the notice in a process, the descriptor table's
     owner, and the others ask it. *Exit*, met by `tty_interrupt` in
     `libc_children`'s QEMU x86-64 boot, the probe being the shell's surface:
     Ctrl+C ends a pipeline whose stage started a child, none of it left and
     the prompt back, and ends no shell nested on the foreground line; a child
     that reads each interrupt survives three Ctrl+C, each pressed once it has
     read the one before, and one that listens and never reads dies on the
     second. Negative control: today's shell, which hands no line (the pipeline
     runs on). Mutations: a quit that kills on a reason already read (the
     reading child dies); a shell that ends on an interrupt (the nested shell);
     and every line asked at once (the reader dies). Oracle: POSIX's INTR, a
     `SIGINT` "sent to all processes in the foreground process group" (XBD
     11.1.9).
   - 7.2 The rest, with stage 5: sshd hangs up a dropped connection's session,
     says what stays and keeps waiting. A stop of the machine is a request
     carrying its mode, wait or terminate. A wait stop asks every session to
     quit with terminate and waits for them until a later terminate request,
     itself the explicit force, ends what stays, or a cancel keeps the machine
     running; test-runner's stops are terminate requests. This much is done
     (`toyos::power`, and `session_lifecycle`'s reboot arm). No product
     timeout makes one; a test's hang ceiling may still end its test with
     one, the test reporting its own stall. `&` is a line the shell watches.
     sshd awaits exits through a ToyOS `tokio/src/process` arm in the tokio fork,
     shaped as its Linux pidfd arm (`process/unix/pidfd_reaper.rs`). *Exit*: a
     dropped SSH connection whose session runs a watching child sees it write
     its file before it ends; tokio's `Child::wait` returns with no loop
     polling `try_wait`. Mutation: a session that never listens (the file
     unwritten). Oracle: tokio's `tests/process_smoke.rs`.
