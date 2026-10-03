---
status: open
kind: track
opened: 2026-10-04
---

# ToyOS explains itself: log, diary, counters, accounting, inspect, sizes and post-mortem on shared foundations

ToyOS answers what it is doing, what it did and what it is made of from
inside itself, with programs it ships. Today the answers are scattered: the
log carries numbers in prose (`irq:`, `tlb:`, `PMM:`, `sched:`, `syscalls:`),
the trace ring has LLDB as its only reader, nothing reads APERF, MPERF,
`MSR_SMI_COUNT`, RAPL or C-state residency, a process's memory is a byte sum
that reads 0 under contention, and a user symbol is resolved by the kernel.
This track is the one plan for all of it; each pillar's own track carries its
steps and its exits, and nothing here restates them.

## The owner's rulings (2026-10-03)

- **Shipped**: "its obvious that we want to be able to understand kernel
  metrics from within toyos ... thats an important and productive tool that
  should be shipped with toyos." Every reader (`trace`, `inspect kernel.*`,
  `size`) is in every image, and access is by rights alone.
- **"Keep crash records"**: ToyOS does not ask the firmware to wipe memory at
  reset; crash records survive a reset in RAM
  (`issues/boot-media/the-loader-never-sets-the-firmwares-memory-overwrite-request.md`,
  `issues/boot-media/a-memory-overwrite-request-ubuntu-left-set-stays-set-under-toyos.md`).
- **"Only with permission"**: reading counters takes a right. The revealing
  ones (power, frequency, per-device interrupt counts and per-CPU wake counts)
  take the strict diary (`trace`) right. Admin tools hold it; toybox does not.
- **"General counters"**: the firmware-interrupt (`MSR_SMI_COUNT`) reading of
  stage 1 of
  `issues/kernel/toyos-runs-the-machine-in-acpi-mode-and-interprets-its-aml.md`
  is built as the first piece of the general counters, not as a one-off check.
- **"Always on"**: the shipped kernel records a system call that runs over the
  threshold in the diary, with its number and its program.
- **Symbols, "adopt"**: the kernel names only itself. A killed program is
  reported as file and offset, and userland names it. The kernel keeps its
  own ELF reader (`toyos-elf`).

Every answer to "where does this run" is ToyOS: the decoder, the reducers, the
symbolizer and every tool run inside ToyOS, and nothing in the build, the tests
or the reading of a record rests on another machine. Linux's readings of the
T14 are oracles recorded as data
(`issues/hardware/linuxs-readings-of-the-t14-and-the-tcg-model-lack-reads-owed-before-the-t14s-wipe.md`).

## The architecture (proposal accepted, 2026-10-03, with the roast's changes)

Four kinds of answer, four places, and no pillar keeps a store of its own:
words go to the **log**, events to the **diary** (the trace ring), numbers
about now to **counters**, the Process object and `inspect`, and static facts
to the ELF files. The kernel records, stamps and counts; userland reduces,
symbolizes and renders. No kernel thread writes a record: the thread or
interrupt that caused it does.

| Pillar | Where it lives | Its track |
|---|---|---|
| Log | the kernel's ring and each program's ring, read by `logkeeper` | `issues/kernel/logging-records-from-every-producer-and-a-kernel-that-waits-on-nobody.md`; the sinks half of `issues/design-debt/redesign-the-log-subsystem.md` |
| Diary | per-CPU rings of 32-byte slots, `SYS_TRACE_READ`, the `trace` tool | `issues/diagnostics/nothing-in-the-machine-can-read-the-trace-ring.md` |
| Counters | one table declared once in `toyos-abi`, per-CPU software words and a per-CPU hardware block | this file |
| Profile | samples are diary records; the sampler is the hard-lockup detector's | behind the diary's judgement; `issues/hardware/a-frozen-toyos-waits-for-a-hand-on-the-power-button.md` arms it every boot |
| Accounting | the Process object (`SYS_PROCESS_STATS`) and the roster | `issues/kernel/nothing-charges-kernel-memory-to-a-process.md`, `issues/kernel/a-processs-memory-is-a-byte-total-that-reads-zero-under-contention.md`, `issues/diagnostics/blocked-time-is-invisible-while-the-park-lasts.md` |
| Inspect | one root per owner over its connector; `kernel.*` from counters, `supervisor.*` from the program table | `issues/diagnostics/the-kernel-keeps-nothing-it-enumerates.md` |
| Sizes | a pure crate over `toyos-elf` behind a toybox `size` applet, and the same crate in `--ci host` | this file |
| Post-mortem | one reset-surviving region holding the black box, the previous boot's log and later its diary | the log's step 1, the diary, this file |
| Symbols | userland names user addresses; the kernel names only its own | `issues/kernel/the-kernel-still-parses-what-userland-writes.md` Move 3, `issues/build/debug-true-produces-no-debug-info.md` |

Shared foundations, which every pillar's track obeys:

- **One clock.** Every stamp is the CPU's free-running counter (TSC,
  `CNTVCT_EL0`); binary records carry raw ticks, text carries nanoseconds by
  `toyos-abi/src/clock.rs`'s formula; a persisted region's header carries the
  clock words.
- **One identity.** pid and tid (`issues/diagnostics/a-record-cannot-name-thread-zero.md`);
  the CPU is implied by the ring; a pid is never authority.
- **One vocabulary per stream, in `toyos-abi`.** A persisted region carries the
  vocabulary itself, not a digest of it. A program's events are keyed records
  in its own log ring, their keys the program's own and opaque to a decoder
  that does not know them.
- **Two reader shapes.** A tap (log, diary): many readers, each its own
  position; a read names one stream and a `u64` position, and the kernel clamps
  it and computes loss saturating, never taking a count from the caller. A
  queue (a program's ring read by `logkeeper`): one reader, and a full ring
  refuses and counts.
- **One right per kind of disclosure**, each right's doc listing what it
  discloses: `log`, `trace` (every schedule, syscall numbers and durations,
  input timing through interrupt drains, sampled addresses, and the revealing
  counters), `counters`, roster, inventory, Process `READ`, and one connector
  per server. `ps` stays on `SYS_SYSINFO`'s ambient header, and no toybox
  applet gains a right before per-applet rows exist
  (`issues/isolation/one-manifest-row-grants-every-applet-and-one-is-compared.md`).
- **One decoder crate**, pure, `no_std` with `alloc`, forbidding unsafe: the
  diary, persisted bytes and counter deltas, with its reducers checked on the
  host against the scheduler simulator (`kernel/sim/src/latency.rs`). The log
  stays with `toyos-logstream`; `ps` and `stats` link neither.
- **One reset-surviving region**, which absorbs the black box under one
  header: the writer's identity and kernel build-id, the clock words, the
  vocabulary, a previous and a current half. The previous boot's bytes are
  handed out raw and decoded in userland as untrusted input.
- **One sampler per CPU**: the PMU-overflow NMI the hard-lockup detector arms.
  The profile and the detector share one counter, at one fixed rate (997 Hz)
  held by a profile handle and reverted when it closes. The NMI writes only
  per-CPU words and a staging array its own CPU moves into the diary at its
  next kernel exit; it never writes a ring and never dereferences a thread. A
  Ring 3 sample raises a self-IPI so its user call chain is walked from the
  sample's own stack. The boot deadline's seal names each CPU's last `rip`
  from the Ring 0 timer frame.
- **One cross-CPU request vector per architecture**, carrying kicks, counter
  requests and the self-IPI; the timer's vector means expiry only. A
  counter request is served by the requester on its own CPU, which then parks
  on a Watch the last answer posts, under a bound; a CPU silent at the bound
  is reported stale, never waited on without a bound and never a panic.
- **Counters are monotonic counts and alloc/free pairs only.** A maximum is
  an over-threshold diary record. A hardware counter is read only where a pure
  function of the CPU's vendor, family and model says it exists, and every
  counter it admits is read on each CPU at bring-up, so a wrong verdict ends
  the boot rather than a user's read.
- **"Now" stays "now".** `inspect` and counters answer now; the log and the
  diary answer what happened. A list of processes or modules is a paged read,
  and a module record is written at an executable file mapping.

## What is to be built, in order

Lane A, what the latency work
(`issues/kernel/toyos-beats-linuxs-latency-on-the-t14.md`) needs: the
diary's tid 0 and the NMI `rflags` defect
(`issues/kernel/the-nmi-entry-can-hand-the-lockup-sample-cs-for-rflags-and-no-test-reds.md`)
ahead of the step that reads them; then the counters with their right, the
request vector and the kick moved onto it, whose T14 exit is that in legacy
mode the SMI count rises alike on every CPU, and through which ACPI stage 1
reads its flatness; then interrupts on in syscalls; then the diary's three
steps and their judgement. Lane B, behind the judgement: the profile, call
chains, the remaining record kinds (waker-to-woken records before usbd). Lane
C, beside them: the rest of the counters (RAPL, C-state residency, thermal),
the log's steps, the accounting fixes, the `inspect` roots, and `size`.

**Exit**: every pillar track above is closed, and at a ToyOS shell on the T14
`inspect kernel.*`, `trace` and `size` answer, each under the right its
manifest row names.
