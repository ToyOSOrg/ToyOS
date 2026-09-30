---
status: owner
kind: question
opened: 2026-09-30
---

# The child-process track waits on the owner's rulings

Stages 2, 3, 6 and 7 of
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
  ToyOS's own event and runs the program's handler for it. Ninja and libuv run
  unchanged, and the kernel gains nothing.
- **Each such program is changed for ToyOS.** Ninja, libuv and every other
  program that waits for its children carries a ToyOS change of its own, as a
  fork, for what one part of libc would serve.

*Recommended: libc imitates it.*

## Q3b. When does a C program's signal handler run? (stages 3 and 6)

On Unix a signal interrupts the program wherever it is, and the handler runs
in its place. ToyOS never does that; the owner ruled it out as legacy. libc can
run a handler only on a thread it controls.

- **At once, beside the program.** While none of the program's threads is
  waiting inside libc, libc runs the handler on a thread of its own, and the
  program's own code goes on running at the same time. Ctrl+C stops clang at
  once, and clang deletes the file it was writing. The cost: a handler that
  jumps back into the program's main loop, as some C programs' do, lands on
  the wrong thread and breaks the program; and clang can delete that file
  while its main code is still writing into it. POSIX lets a handler run on
  any of a program's threads that does not block the signal; this one is a
  thread the program never made.
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
  hands its child a file by leaving it open, without naming it, loses it. None
  of Ninja, LLVM or libuv does that.
- **Unix's rule.** A C parent with any other file open passes it on, and every
  program it starts runs with what that parent holds, where the same program
  started from Rust runs with what it is declared to hold.

*Recommended: only what the parent names.*

## Q6a. Can one program ask another to quit? (stage 6)

Today the only way one program can end another is a kill, which leaves it no
chance to save anything.

- **Yes, with a reason.** A parent can ask its child to quit and say why:
  interrupt (Ctrl+C), hang-up (a window closed, a connection dropped) or
  terminate (shutdown, a deadline). The program hears it among its other
  events and decides what to do: an editor saves, a compiler deletes its
  half-written output, Ctrl+C in a shell or an editor stops what it is doing
  and ends nothing, and a server may take a hang-up as the signal to reload
  its settings. Whoever asked still kills if the end it wants does not come.
  The kernel gains one call and one kind of object.
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
  first press.
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
  by putting the helper in a job of its own.
- **The program alone.** Ctrl+C reaches `make` and not its compiles, and make,
  which expects its compiles to have been interrupted with it, waits for every
  running one to finish before it stops (GNU make's `fatal_error_signal`). A
  closing terminal reaches its shell and not what the shell started.

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
  program a quit ended reads as ended for that reason, and a C parent sees the
  Unix signal for it. For clang to read that way, a program must be able to
  end itself for one of those reasons, as a Unix program can by sending the
  signal to itself, so the number no longer says only what the kernel did.

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
  that takes each Ctrl+C and carries on survives any number of them, as on
  Unix; a program stuck so that it never takes the first dies on the second.
  A program that takes a Ctrl+C and then hangs is not ended by Ctrl+C:
  closing its window or a kill ends it, as on Unix.
- **It always kills.** A hung program always dies on the second press, but a
  Python prompt or an editor dies on the second Ctrl+C of its life, and loses
  what was not saved.

*Recommended: only a program that has not yet taken the first.*
