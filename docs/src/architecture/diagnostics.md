# Logs and diagnostics

ToyOS answers what it is doing, what it did and what it is made of from inside
itself, with programs it ships. Diagnostics stay on the machine until someone
exports them.

## The log

```text
  program ──> its own log ring ─┐
  program ──> its own log ring ─┼──> log keeper ──> /log, one file per boot
  kernel  ──> kernel record ring┘          └──> console
```

- Every program the supervisor starts or launches writes records into a log
  ring of its own, which the supervisor names to the log keeper. The name
  travels beside the ring, so a line's origin is structure, not a claim the
  program makes. Writing never waits, allocates or enters the kernel; a full
  ring becomes a count the log keeper reports.
- The kernel's logging responsibility is its own record ring and its console.
  Userland output never enters the kernel ring. Reading the machine's log
  needs a right on the system capability.
- The log keeper owns the log files. It holds each program to an allowance,
  keeps every boot's beginning, and syncs on an alert or an interval. Severity
  is an ordered ladder.
- A slow sink drops and counts; it never blocks a producer and fails alone.
- The kernel waits on the log keeper neither at a panic nor at a stop.

## Crashes

A kernel panic halts the other CPUs, paints its report on the panel and seals
its tail in a black-box page that survives the reset, which the next boot
carries into `/log`. A machine with no serial port can therefore always be read
afterwards. A killed program is reported as its file, offset and build id; one
userland naming service turns those into function names using the same lookup
the kernel uses for itself.

## Inspecting a running machine

- **Inspect**: each owner of state, the kernel and the services, answers
  questions about itself, and one reader renders the answers.
- **Counters**: the kernel's counters are read on demand. Reading them needs a
  right; the revealing ones, such as power and per-device interrupts, need a
  stricter one.
- **The diary**: per-CPU trace records of scheduling and system calls, always
  on in the shipped kernel, including every system call slower than a
  threshold.
- **Process figures**: what each process uses, for those holding the right to
  list processes.

Ordinary programs receive none of these rights by default; administrative tools
do.
