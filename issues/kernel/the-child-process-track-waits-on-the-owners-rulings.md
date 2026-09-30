---
status: owner
kind: question
opened: 2026-09-30
---

# The child-process track waits on the owner's rulings

Stages 2, 3, 6 and 7 of
`issues/kernel/a-childs-end-is-an-event-and-a-parent-takes-its-children-down.md`
wait on these. Each ruling goes into the track as one line, and its entry here
is deleted.

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

**`SIGCHLD`.** libc raises it once for each end of a child it started, which
its own thread learns from stage 1's event. libc runs a handler for any signal
it imitates where POSIX would: inside a blocking libc call — `ppoll` under its
mask, or `poll` — of a thread that leaves the signal unblocked, and that call
then answers `EINTR`; while no such thread is in one, at once on libc's own
thread, as POSIX lets a signal sent to a process run on any thread that does
not block it; and while every thread blocks it, it stays pending, which
`sigpending` reports, until a call unblocks it. Once the handler has run for
an end, that end never raises it again, even while the child is not yet waited
for. With `SIGCHLD` set to `SIG_IGN`, or `SA_NOCLDWAIT` asked for, libc drops
an ended child without a wait, as POSIX says, so it stops counting against how
many children a process may have.
`sigaction` records handlers, where today it answers `0` and records nothing.
This is what the build tools use: Ninja blocks `SIGCHLD` except inside `ppoll`
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

## Q6. Asking a program to quit (stages 6 and 7)

Today the only end one program can cause another is a kill, which leaves it no
chance to save, and quitting gracefully is a parent's job before it ends.
Proposed: two operations on the one handle a parent holds — quit and kill.

- **Quit** is a new syscall, `SYS_PROCESS_QUIT` = 124,
  `(process: RawHandle, reason) -> ()`, needing `MANAGE` as the kill does, and
  `Ok` for a process already ended. The reason is interrupt (Ctrl+C), hang-up
  (a closed window, a dropped connection) or terminate (shutdown, a deadline),
  which a program needs as much as libc: Ctrl+C in a shell or an editor stops
  what it is doing and ends nothing. It interrupts nothing: each process is
  started holding a quit notice, a handle under the label `quit` that is
  `READABLE` while a reason is asked and not yet read, and the program waits
  on it beside any other event.
- A program that has never watched its notice is killed by a quit instead,
  with its tree, as a Unix program with no handler dies of Ctrl+C at once.
- Whoever asks kills when the end it wants has not come. The terminal asks
  with interrupt on Ctrl+C and kills on a second; a closed window and a
  dropped SSH session ask with hang-up, shutdown and a test past its deadline
  with terminate, each killing at a deadline of its own.
- std: `os::toyos` hands a program its notice — a blocking wait for a reason,
  and the handle for a poller. The `ctrlc` fork `rust/Cargo.toml` patches in,
  whose ToyOS arm parks its waiting thread forever today, waits there
  instead, for the reasons its Unix arm maps; and rustc stops skipping its
  handler on ToyOS (`rust/compiler/rustc_driver_impl/src/lib.rs`,
  `install_ctrlc_handler`). An unchanged Rust program's `ctrlc` handler then
  runs on a quit.
- libc: from the first `sigaction` that installs a handler for `SIGINT`,
  `SIGHUP` or `SIGTERM`, libc's own thread watches the notice, and each reason
  runs its signal's handler by Q3's rule — interrupt `SIGINT`, hang-up
  `SIGHUP`, terminate `SIGTERM`. A reason whose signal is ignored is dropped,
  and one with neither handler nor ignore takes the default action, which ends
  the process with exit code 128 plus the signal's number: `waitpid` reads it
  as an exit, not `WIFSIGNALED`. So a quit ends clang at once: LLVM's handler
  removes its output files, restores the default action and raises the signal
  again (`rust/src/llvm-project/llvm/lib/Support/Unix/Signals.inc`,
  `SignalHandler`). A handler that only sets a flag leaves the program running
  until it acts on the flag, and a daemon's `SIGHUP` reload stays a reload.
  `kill` with one of the three is a quit with that reason, and with `SIGKILL`
  a kill.

What it adds to the ABI: the syscall (124, never assigned), the notice — a
fourteenth object kind, three reason bits, a read that takes them, and a
watch — and its label, installed in every process the kernel starts. The kill
is unchanged.

*Recommended.* Rejected:

- A quit channel std, libc and the SDK make at every spawn, a pipe whose read
  end the child holds: no kernel change, but two handles travel wherever one
  did, every launch carries one more, a program started any other way has
  none, and one that never listens ends only when its asker's deadline runs
  out.
- A quit with no reason: a daemon that reloads on `SIGHUP` and exits on
  `SIGTERM` would exit when asked to reload, and no program could tell Ctrl+C
  from shutdown.
- Quit as a second mode of the kill (109): one number for two operations with
  different effects.
- A signal delivered into the program's own code, a handler that interrupts
  whatever it was doing: the legacy the owner ruled out.
