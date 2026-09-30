---
status: owner
kind: question
opened: 2026-09-30
---

# The child-process track waits on the owner's rulings

What `issues/kernel/a-childs-end-is-an-event-and-its-tree-is-a-job.md` builds
from stage 2 on waits on these. Each ruling goes into the track as one line,
and its entry here is deleted.

## Q2. How a program ended (stage 2)

When a child ends, its parent reads one number. Today a child that exits with
137 and one that is killed both read 137, and a crash reads 139 or -1, numbers
a program can exit with too: nobody can tell a failure from a kill from a
crash.

Proposed: `SYS_PROCESS_WAIT` (108) keeps its number, arguments, right (`WAIT`)
and errors, and its one answer word says what happened.

- Bit 32 is 0 if the program exited and 1 if the kernel ended it.
- Bits 0–31 are its exit code if it exited. If the kernel ended it they say
  why, and are never 0: 1 killed, 2 a memory access its mappings refuse, 3 an
  instruction it may not run, 4 an arithmetic trap, 5 a misaligned access, 6 a
  breakpoint instruction, 7 a handle it does not hold. The kind is what the
  program did, so the same act is the same kind on x86-64 and AArch64, whatever
  each crash report prints.
- Bits 33–63 stay 0, clear of the error range.

A reader still decoding bits 0–31 alone reads a killed or crashed program as a
failure, never as a success. 137, 139 and -1 go; an abort stays the program's
own exit code, 134 from std and from libc. std's `code()` answers `None` for a
program the kernel ended, and a ToyOS `ExitStatusExt` reads why. libc's
`waitpid` reports it as `WIFSIGNALED`, `WTERMSIG` being `SIGKILL`, `SIGSEGV`,
`SIGILL`, `SIGFPE`, `SIGBUS`, `SIGTRAP` or `SIGSYS` for 1 to 7.

*Recommended.* Rejected: the shell's numbers inside the code, which read a
program's own `exit(137)` as a kill; and a struct written out through a
pointer, a copy for what one word carries.

## Q3. `SIGCHLD`, and what a C child inherits (stage 3; no ABI change)

ToyOS has no signals, and its kernel never will: nothing is ever sent from one
process to another as a signal. A signal exists only inside libc, imitated for
the C programs written around them, out of ToyOS events libc sees itself.

**`SIGCHLD`.** libc raises it once for each end of a child it started, which it
learns from stage 1's event. It runs the program's handler only while the
program is blocked in a libc call that leaves `SIGCHLD` unblocked — `ppoll`
under its mask, or `poll` — and that call then answers `EINTR`, as POSIX says.
Once the handler has run for an end, that end never raises it again, even
while the child is not yet waited for. With `SIGCHLD` set to `SIG_IGN`, or
`SA_NOCLDWAIT` asked for, libc drops an ended child without a wait, as POSIX
says, so it stops counting against how many children a process may have.
`sigaction` records handlers, where today it answers `0` and records nothing. A
program that waits for `SIGCHLD` without blocking in libc never gets it. This
is what the build tools use: Ninja blocks `SIGCHLD` except inside `ppoll`
(v1.13.1 `src/subprocess-posix.cc`), and libuv, without kqueue, learns of a
child from a handler that writes its own pipe (v1.53.0 `src/unix/process.c`),
which an `EINTR` from `poll` serves.

**What a C child inherits.** POSIX hands a child every descriptor its parent
holds that is not marked close-on-exec, and `open` and `pipe` leave them
unmarked by default. Proposed instead, as a stated deviation from POSIX: libc
passes descriptors 0–2 and exactly what the spawn's file actions name. The
reason is std's routing rule (`rust/library/std/src/sys/process/toyos.rs`,
`Command::spawn`): a child handed a slot beyond 0–2 is spawned directly,
holding what its parent holds, and any other is started by init from its own
manifest row. Under POSIX's default, a C parent holding any unmarked descriptor
above 2 would start every program the first way while a Rust parent starts it
the second; under this rule a C and a Rust parent give one program the same
authority. None of Ninja, LLVM or libuv hands a child a descriptor above 2
without naming it. The cost: a C program that hands its child a descriptor by
leaving it open, without naming it, loses it.

*Recommended, both.* Rejected: a ToyOS backend in each program that waits for
children (libuv's kqueue shape would fit the handle's event), a fork per
program for what one libc routine serves; and POSIX's default inheritance, for
the split in authority above.

## Q4. One mechanism: a program owns what it starts (stage 4)

The track takes this as the one mechanism the owner asked for; the ruling asked
is whether it stands. Today a kill ends one process, and what it started runs
on.

- A program owns what it starts. Killing a process ends it, everything it
  started and everything those started; so does the process ending on its own,
  however it ends. A parent's wait for a child completes once the child's whole
  tree is gone.
- The `Process` handle is the only control. The kernel remembers which process
  started which, only to find a tree; no call takes a pid, and being a parent
  grants nothing — holding the handle does.
- A program started through the launcher belongs to whoever asked, so nothing
  the shell starts escapes a Ctrl+C or a closed window: each process holds a
  handle to itself that std and libc send with every launch, so a launch costs
  one handle copy, and no object or nesting level. A service
  (`service = true` in `system.toml`) belongs to init, however it is started.

It serves each case: Ctrl+C ends what the shell started for the line, and what
those started; a closed window ends the terminal, its shell and everything
under them; a dropped SSH session ends that session's process and its tree; a
test past its deadline is killed with everything it started, and the next test
starts once all of it is gone; Ninja's `kill(-pgid)` ends that command's
process and its tree, and nothing Ninja started outlives Ninja.

What changes, plainly:

- A kill reaches the whole tree: std's `Child::kill` and libc's
  `kill(pid, SIGKILL)` end the child's descendants too, where Unix ends the
  child alone.
- Nothing outlives what started it but a service. A helper a program starts
  and leaves running when it ends — a server started with `&` from a script
  that then exits, sccache's server, tmux's server — ends with it; what must
  outlive its starter is a service. `/system/bin/sshd &`, the way `system.toml`
  says sshd is started by hand, keeps working: sshd is a service, so it runs
  under init.

*Recommended.* Rejected:

- A job object beside the process: a fourteenth object kind,
  `SYS_JOB_CREATE`, a job in `SpawnArgs`, and a kill that takes either. A
  process holds no handle to its own job, so std and libc would create one
  before every launch: a job object and a nesting level per hop, and a chain of
  launches deeper than 32 refused. Two things to kill, two to watch, two places
  a child lands.
- One mechanism where a process that exits leaves what it started running: its
  end would then say nothing about what it left, so a supervisor that kills the
  rest — init replacing a service whose leftover still holds its device,
  test-runner before the next test — would need a second event to learn it is
  gone.
- A process killed when its last handle closes (`pdfork` without `PD_DAEMON`):
  Rust documents that a dropped `Child` "will continue to run", and here it
  does, until its parent ends.
- The parent relation as authority, which ToyOS retired with `SYS_KILL` (65):
  here it only says how far a kill reaches.

## Q6. Asking a program to quit (stage 5)

Today the only end one program can cause another is a kill, which leaves it no
chance to save. Proposed: two operations on the one handle a parent holds —
quit and kill.

- **Quit** is a new syscall, `SYS_PROCESS_QUIT` = 124,
  `(process: RawHandle) -> ()`, needing `MANAGE` as the kill does, and `Ok` for
  a process already ended or already asked. It interrupts nothing: each process
  is started holding a quit notice, a handle under the label `quit` that
  becomes `READABLE` once its process is asked to quit, and the program waits
  on it beside any other event, then saves and exits.
- A program that has never watched its notice is killed by a quit instead,
  with its tree, as a Unix program with no handler for `SIGINT` dies of Ctrl+C
  at once.
- Whoever asks waits for the program's end with a deadline of its own and
  kills it if the end has not come. The terminal asks on Ctrl+C and kills on a
  second Ctrl+C or at its deadline; a closed window, a dropped SSH session,
  shutdown and a test past its deadline are the same pair.
- libc: a handler for `SIGINT`, `SIGTERM` or `SIGHUP` watches the notice, and a
  quit runs the first of those the program handles inside a blocking libc
  call, as `SIGCHLD` does; `kill` with `SIGINT`, `SIGTERM` or `SIGHUP` is a
  quit, and with `SIGKILL` a kill. std: the SDK hands a program its notice to
  register in its poller.

What it adds to the ABI: the syscall (124, never assigned), the notice — a
fourteenth object kind, one bit and a watch — and its label, installed in every
process the kernel starts. The kill is unchanged.

*Recommended.* Rejected:

- A quit channel std, libc and the SDK make at every spawn, a pipe whose read
  end the child holds: no kernel change, but two handles travel wherever one
  did, every launch carries one more, a program started any other way has
  none, and one that never listens ends only when its asker's deadline runs
  out.
- Quit as a second mode of the kill (109): one number for two operations with
  different effects.
- A signal delivered into the program's own code, a handler that interrupts
  whatever it was doing: the legacy the owner ruled out.
