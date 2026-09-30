---
status: owner
kind: question
opened: 2026-09-30
---

# The child-process track waits on the owner's rulings

Stages 2, 3, 5, 6 and 7 of
`issues/kernel/a-childs-end-is-an-event-and-a-parent-takes-its-children-down.md`
wait on these, and are written there as recommended here. Each question is one
decision. Its ruling goes into the track as one line, and its entry here is
deleted.

## Q2. Can a parent tell a failure from a kill or a crash? (stage 2)

When a child ends, its parent reads one number. Today a child that exits with
137 and one that is killed both read 137, and a crash reads 139 or -1, numbers
a program can exit with too.

- **Yes.** The number says whether the program ended itself or the kernel
  ended it, and if the kernel did, why: killed, or which kind of crash. A crash
  reads the same on x86-64 and on ARM64. A program that reads the number the
  old way still sees a kill or a crash as a failure, never as a success.
- **No.** A tool that retries a crashed job cannot tell it from one that
  failed, and a program that exits with 137 on purpose reads as killed.

*Recommended: yes.*

## Q3a. How does a C program learn that its child ended? (stage 3)

C programs written for Unix learn it from a Unix signal, `SIGCHLD`: Ninja
waits for one (v1.13.1 `src/subprocess-posix.cc`), and so does libuv where it
has no kqueue (v1.53.0 `src/unix/process.c`). ToyOS has no signals, and its
kernel never will.

- **libc imitates the signal.** libc, the C library, learns of the end from
  ToyOS's own event and runs the program's handler for it. Ninja runs
  unchanged, and the kernel gains nothing.
- **Each such program is changed for ToyOS.** Ninja, libuv and every other
  program that waits for its children carries a ToyOS change of its own, as a
  fork, for what one part of libc would serve.

*Recommended: libc imitates it.*

## Q3b. When does a C program's signal handler run? (stages 3 and 6)

On Unix a signal interrupts the program wherever it is, and the handler runs
in its place. ToyOS never does that; the owner ruled it out as legacy. libc can
run a handler only on a thread it controls.

- **At once, beside the program.** While one of the program's threads is
  waiting inside libc — for input, for a child, for time to pass or for a
  signal — the handler runs on that thread and cuts the wait short, as on
  Unix: a C prompt waiting for a line comes back on Ctrl+C. As on Unix, a
  program can ask that a wait for input or for a child carry on after its
  handler instead, and a wait for time or for a signal is always cut short.
  While no thread is waiting, libc runs the handler on a thread of its own, and
  the program's own code goes on running at the same time. Ctrl+C stops clang
  at once, and clang deletes the file it was writing. The cost: the handler and
  the program's own code can then work on the same thing at once and collide.
  GNU make's Ctrl+C handler collects the compiles that have ended, which make's
  own code also does; on Windows, the one system where make's handler already
  runs beside its code, make stops its own code first for exactly that reason
  (make 4.4.1 `src/commands.c:508-510`). On ToyOS it collides only when Ctrl+C
  comes while make is working rather than waiting. A handler that jumps back
  into the program's main loop, as some C programs' do, lands on the wrong
  thread and breaks the program; and clang can delete that file while its main
  code is still writing into it. POSIX lets a handler run on any of a program's
  threads that does not block the signal; this one is a thread the program
  never made.
- **Only when the program next waits inside libc.** Nothing runs beside the
  program. A compiler that is computing does not wait, so Ctrl+C does not stop
  clang until it is killed, and its half-written file stays behind.

*Recommended: at once, beside the program.*

## Q3c. What does a C child start with? (stage 3)

On Unix a child starts holding every file its parent has open, unless the
parent marked the file not to be passed on.

- **Only what the parent names**, a stated departure from POSIX. A C child
  starts with its input, output and error, and exactly what its parent names
  when it starts it. A program started from C then holds what it holds when
  started from Rust: what it is declared to hold. The cost: a C program that
  hands its child a file by leaving it open, without naming it, loses it. GNU
  make does that to share its limit on how many jobs run at once, with a pipe
  it leaves open to a make it starts; LLVM's tools, told to share the limit,
  take the same pipe (`llvm/lib/Support/Unix/Jobserver.inc`). Without it, a
  make that make starts warns and runs one job at a time (make 4.4.1
  `src/main.c:1849`), and an LLVM tool warns and runs as many as it would
  alone. make shares the limit by a named pipe instead where the system has
  one, and ToyOS has none.
- **Unix's rule.** A C parent with any other file open passes it on, and every
  program it starts runs with what that parent holds, where the same program
  started from Rust runs with what it is declared to hold.

*Recommended: only what the parent names.*

## Q5a. What program is a login? (stage 5)

Each login, on the desktop or over SSH, has one program that is the parent of
everything its user starts, so that logging out ends all of it.

- **A program of its own that does nothing else.** init starts a small session
  program for each login, and everything the user starts runs under it. It
  listens: when the login is asked to end, its programs are asked too, and it
  ends once they have. A crash of any program the user started leaves the
  login and the rest of its programs running.
- **The login's first program.** An SSH login's session is the shell sshd
  starts for it, and the desktop's is the first program the desktop starts for
  the user. When that program ends or crashes, everything the user started
  ends with it, as on Unix when the login shell exits. One program fewer runs
  per login.

*Recommended: a program of its own.*

## Q5b. Who lets the compositor and sshd start programs in a login? (stage 5)

A login's programs are started under its session, never under the compositor
or sshd, so each of them needs a right to start programs there.

- **init hands it over when it starts the session.** At boot init starts the
  desktop's session and gives the compositor the right to start programs in
  it; for SSH, sshd asks init for a session per connection and gets that right
  back. Each right reaches one session.
- **The session program hands it over itself**, over a connection init sets up
  between them. The session program then has a second job besides being a
  parent, and each login costs one connection more.

*Recommended: init hands it over.*

## Q5c. Can sshd end a login it asked for? (stage 5)

When an SSH connection drops, its login ends: its programs are asked to hang
up, which reaches them because the session program listens (Q5a), and what is
left is killed.

- **Yes, only its own.** The right sshd gets for a connection's session also
  lets it ask that session to quit and kill it, so a dropped connection ends
  its login with nobody else involved. sshd can end no other login.
- **No, it asks init.** Only init can end a login: sshd tells init that the
  connection dropped, and init ends it. init does one more job, and stays the
  only program that can end a login.

*Recommended: yes, only its own.*

## Q6a. Can one program ask another to quit? (stage 6)

Today the only way one program can end another is a kill, which leaves it no
chance to save anything. The same answer decides how init asks each service to
finish when the machine shuts down
(`issues/isolation/the-supervisor-is-host-tested-and-owns-the-stop.md`).

- **Yes, with a reason.** A program allowed to end another — its parent, or
  the terminal it runs in — can ask it to quit and say why: interrupt
  (Ctrl+C), hang-up (a window closed, a connection dropped) or terminate
  (shutdown, a deadline). The program hears it among its other events and
  decides what to do: an editor saves, a compiler deletes its half-written
  output, Ctrl+C in a shell or an editor stops what it is doing and ends
  nothing, and a server may take a hang-up as the signal to reload its
  settings. The shell, the terminal and a login's session program listen, so
  the programs under them are asked too. Whoever asked still kills if the end
  it wants does not come. The kernel gains one call and one kind of object.
- **Yes, without a reason.** A server that reloads on a hang-up and exits on
  terminate would exit when asked to reload, and no program could tell Ctrl+C
  from shutdown.
- **No: a kill only, as today.** Nothing saves its work when its window closes
  or the machine shuts down.

*Recommended: yes, with a reason.* Rejected: a quit channel std and libc would
make at every start, which a program started any other way would not have; and
a handler that interrupts the program's own code wherever it is, the legacy
the owner ruled out.

## Q6b. What does a quit do to a program that never listens for one? (stage 6)

Most programs never listen: `ls`, `cat`, a Rust program that does not ask.

- **It is killed at once, with everything it started**, as a Unix program
  that handles nothing dies of Ctrl+C. Ctrl+C stops such a program on the
  first press. The cost: a program that listens still dies at once, without
  cleaning up, when the program that started it does not listen, because it
  dies with that program; on Unix only the one that does not listen would die.
  ToyOS's own shell and terminal and each login's session program listen, so
  a compiler run from a login cleans up.
- **Nothing, until whoever asked gives up and kills it.** Ctrl+C does nothing
  to `cat` until a second press, and shutdown waits out its whole deadline for
  every program that never listens.

*Recommended: killed at once.*

## Q6c. How far does a quit reach? (stages 6 and 7)

- **The program and everything it started**, as a kill does. Ctrl+C reaches
  `make` and every compile it is running at once, as on Unix, where Ctrl+C
  reaches every program of the job in front. A closing terminal asks its shell
  and everything the shell started to hang up. The cost: a program cannot keep
  a helper it started out of a Ctrl+C meant for itself, as a Unix program can
  by putting the helper in a job of its own. Ninja does that with every
  compile and passes the Ctrl+C on to them itself (v1.13.1
  `src/subprocess-posix.cc:102`, `:451-457`), so here each compile is asked
  twice, and one that has not yet taken the first when Ninja's comes is
  killed (Q7), leaving its half-written file. The shell and the session
  program pass nothing on, since the quit already reached what they run.
- **The program alone.** Ctrl+C reaches `make` and not its compiles, and make,
  which expects its compiles to have been interrupted with it, waits for every
  running one to finish before it stops (GNU make's `fatal_error_signal`). The
  shell and the session program must then pass each quit on to what they run,
  or a closing terminal reaches its shell and not what the shell started.

*Recommended: the program and everything it started.*

## Q6d. Does a program that Ctrl+C ended read as interrupted? (stages 2 and 6)

Under the answers recommended here, no child that Ctrl+C ended ever reads to
its parent as interrupted. One that never listened reads as killed; a C
program that lets the interrupt end it once it has cleaned up, as clang does,
reads as having exited with 130, which is a failure.

- **No.** The number keeps meaning only whether a program ended itself or the
  kernel ended it. A build tool that tells an interrupt from a failure, as
  Ninja does, may report a compile the user interrupted as failed; Ninja still
  stops the build, because the Ctrl+C reached it too.
- **Yes.** Q2's reasons gain three — interrupted, hung up, terminated — so a
  program a quit ended reads as ended for that reason, and a C program that
  started it sees what it would on Unix. For clang to read that way, a program
  must be able to end itself for one of those reasons, as a Unix program can
  by sending the signal to itself, so the number no longer says only what the
  kernel did.

*Recommended: no.*

## Q6e. Does an unchanged Rust program's Ctrl+C handler run? (stage 6)

Rust programs that handle Ctrl+C mostly do it through one widely used library,
`ctrlc`; rustc is one of them.

- **Yes.** Rust's standard library hands a program its quit notice, and the
  `ctrlc` fork ToyOS already carries listens to it, so a Rust program's handler
  runs on a quit as it does on Unix, with no change to the program. rustc
  stops leaving its handler out on ToyOS.
- **No.** A Rust program hears a quit only through a call written for ToyOS;
  rustc and every other `ctrlc` user is treated as a program that never
  listens (Q6b).

*Recommended: yes.*

## Q6f. Does a C program hear a quit as the Unix signal for it? (stage 6)

- **Yes.** libc turns interrupt into `SIGINT`, hang-up into `SIGHUP` and
  terminate into `SIGTERM`, so a C program written for Unix acts as it does
  there: clang deletes the file it was writing and stops, a server reloads on
  a hang-up, and a program that says only to ignore Ctrl+C runs on through
  it. A C program's `kill` with one of those signals asks for a quit.
- **No.** A C program never hears a quit, and is treated as a program that
  never listens (Q6b): clang leaves its half-written file behind, and a server
  asked to reload ends.

*Recommended: yes.*

## Q7. What does a second Ctrl+C do? (stage 7)

A program that listens may stop what it is doing on Ctrl+C and carry on: a
Python prompt goes back to its prompt, an editor cancels a command.

- **It kills only a program that has not yet taken the first.** A program
  that takes each Ctrl+C and carries on survives any number of them; one stuck
  so that it never takes the first dies on the second. The same holds whoever
  asks: a program asked to quit again before it took the last ask is killed.
  Two quick presses: a C program, or a Rust program using `ctrlc`, takes each
  on a thread of its own as it comes, so it survives them unless the machine
  is too busy to run that thread between the two; a program that takes its
  Ctrl+C in its own loop and is busy across both presses dies, where on Unix
  its handler would have run twice. A program that takes a Ctrl+C and then
  hangs is not ended by Ctrl+C: closing its window or a kill ends it, as on
  Unix.
- **It always kills.** A hung program always dies on the second press, but a
  Python prompt or an editor dies on the second Ctrl+C of its life, and loses
  what was not saved.

*Recommended: only a program that has not yet taken the first.*
