---
status: open
kind: track
opened: 2026-10-04
---

# ToyOS explains itself: log, diary, counters, accounting, inspect, sizes and post-mortem

ToyOS answers what it is doing, what it did and what it is made of from
inside itself, with programs it ships. Today the answers are scattered: the
log carries numbers in prose (`irq:`, `tlb:`, `PMM:`, `sched:`, `syscalls:`),
the trace ring has LLDB as its only reader, nothing reads APERF, MPERF,
`MSR_SMI_COUNT`, RAPL or C-state residency, a process's memory is a byte sum
that reads 0 under contention, and a user symbol is resolved by the kernel.
Each pillar's own track carries its steps and its exits; this track holds the
counters and sizes pillars, which no other track does.

## The owner's rulings (2026-10-03)

- On the diary's reader: "its obvious that we want to be able to understand
  kernel metrics from within toyos ... thats an important and productive tool
  that should be shipped with toyos."
- **"Keep crash records"**: ToyOS does not ask the firmware to wipe memory at
  reset, and crash records survive a reset in RAM
  (`issues/boot-media/the-loader-never-sets-the-firmwares-memory-overwrite-request.md`).
- **"Only with permission"**, the option he chose: "A program needs a
  permission to read counters; the revealing ones (power, per-device
  interrupts) need the strict diary permission. Admin tools get them; ordinary
  programs and the toolbox don't until each program can be granted rights on
  its own."
- **"General counters"**: the firmware-interrupt (`MSR_SMI_COUNT`) reading of
  stage 1 of
  `issues/kernel/toyos-runs-the-machine-in-acpi-mode-and-interprets-its-aml.md`
  is built as the first piece of the general counters, not as a one-off check.
- **"Always on"**: the shipped kernel records a system call that runs over the
  threshold in the diary, with its number and its program.
- **Symbols, "adopt"**: the kernel names only itself; a killed program is
  reported as file and offset, and one userland naming service shares the
  kernel's lookup and demangling code. He asked that it never panic or fail.

## Decided by the orchestrator, not ruled (2026-10-03)

From the orchestrator's strategy and the roast of it that the orchestrator
adopted. A line marked *told* was told to the owner with his veto open; the
rest were not put to him.

- *Told*: counters are read on demand. The roast's bound: the requester serves
  its own CPU and parks on a Watch under a bound, and a CPU silent at the
  bound reads as stale.
- *Told*: one sampler per CPU, the hard-lockup detector's PMU NMI, at one
  fixed rate held by a profile handle.
- The kick leaves the timer's vector for one cross-CPU request vector, which
  counter requests share.

| Pillar | Its track |
|---|---|
| Log | `issues/kernel/logging-records-from-every-producer-and-a-kernel-that-waits-on-nobody.md`; the sinks half of `issues/design-debt/redesign-the-log-subsystem.md` |
| Diary | `issues/diagnostics/nothing-in-the-machine-can-read-the-trace-ring.md` |
| Counters, sizes | this file |
| Profile | behind the diary's judgement; `issues/hardware/a-frozen-toyos-waits-for-a-hand-on-the-power-button.md` |
| Accounting | `issues/kernel/nothing-charges-kernel-memory-to-a-process.md`, `issues/kernel/a-processs-memory-is-a-byte-total-that-reads-zero-under-contention.md`, `issues/diagnostics/blocked-time-is-invisible-while-the-park-lasts.md` |
| Inspect | `issues/diagnostics/the-kernel-keeps-nothing-it-enumerates.md` |
| Post-mortem | step 1 of `issues/kernel/logging-records-from-every-producer-and-a-kernel-that-waits-on-nobody.md` |
| Symbols | `issues/kernel/the-kernel-still-parses-what-userland-writes.md` Move 3 |

## What is to be built, in order (the orchestrator's)

Lane A, what `issues/kernel/toyos-beats-linuxs-latency-on-the-t14.md` needs:
the diary's tid 0 and
`issues/kernel/the-nmi-entry-can-hand-the-lockup-sample-cs-for-rflags-and-no-test-reds.md`;
then the counters with their rights and the request vector, whose T14 exit is
that in legacy mode the SMI count rises alike on every CPU; then interrupts on
in syscalls; then the diary's three steps. Lane B, behind the diary's
judgement: the profile and call chains. Lane C: RAPL, C-state residency and
thermal counters, the log's steps, accounting, `inspect` and `size`.

**Exit**: every pillar track above is closed; at a ToyOS shell on the T14
`inspect kernel.*`, `trace` and `size` answer, each under the right its
manifest row names; a program without the counters right is refused a
counter read, and one that holds it but not `trace` is refused the power and
per-device interrupt counters; and a T14 row holds each counter ToyOS reads
there against Linux's reading of it
(`issues/hardware/linuxs-readings-of-the-t14-and-the-tcg-model-lack-reads-owed-before-the-t14s-wipe.md`).
