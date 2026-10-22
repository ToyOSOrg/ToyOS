---
status: open
kind: track
opened: 2026-10-04
---

# ToyOS explains itself: log, diary, counters, accounting, inspect, sizes and post-mortem

ToyOS answers what it is doing, what it did and what it is made of from
inside itself, with programs it ships. Today the answers are scattered: the
log carries numbers in prose (`irq:`, `tlb:`, `syscalls:`),
the diary computes no lateness, nothing reads RAPL or C-state
residency, and a process's memory is a byte sum that reads 0 under
contention.
Each pillar's own track carries its steps and its exits; this track holds the
counters and sizes pillars, which no other track does.

## The owner's rulings

- **AI-assisted troubleshooting** (2026-10-18): the owner expects an agent to
  perform troubleshooting. Diagnostic and repair tools must be usable by an
  ordinary authorized program, under the same capability rules as any other
  program. This grants no special AI authority or automatic diagnostic upload.
  Owner refinement, 2026-10-18: cloud AI is sufficient; offline AI and a local
  model/inference runtime are not required. Local diagnostic and recovery tools
  must remain usable without the agent, with explicit report export for analysis
  elsewhere. Agent delivery and concrete failure paths remain design work in
  the ordinary permission model; no bundled agent is
  commissioned by this direction alone.
  Owner refinement, 2026-10-20: files, terminals and diagnostic APIs suffice
  for first-release development and troubleshooting. General desktop
  automation is deferred; ordinary user-facing graphical controls remain.

- **Basic graphical controls for release one** (2026-10-18): users must be
  able to inspect resource use and stop apps through graphical system controls.
  Detailed profiling may remain in developer tools. Respect inspection and
  termination authority across user sessions; define metrics and overload
  usability under pressure without requiring detailed profiling in the GUI.
- **Local reporting only for release one** (2026-10-18): diagnostics remain on
  the machine unless the user explicitly exports and shares them. No automatic
  crash reporting, including opt-in uploads, is required for the first release.
  Owner refinement, 2026-10-18: smaller crash reports by default; capture a
  program's memory only when detailed troubleshooting is explicitly enabled.
  Smaller reports/logs may still contain sensitive data. Exact contents, opt-in
  scope, retention and export/access controls remain to design. This does not
  override the reset-memory-wiping rule below.
- On the diary's reader: "its obvious that we want to be able to understand
  kernel metrics from within toyos ... thats an important and productive tool
  that should be shipped with toyos."
- **Memory wiping takes priority** (2026-10-17): request supported firmware
  wiping after unexpected resets, accepting loss of unsaved crash records.
  This replaces the earlier RAM-retention decision; diagnostics must not
  disable or bypass the mitigation
  (`issues/the-loader-never-sets-the-firmwares-memory-overwrite-request.md`).
- **"Only with permission"**, the option he chose: "A program needs a
  permission to read counters; the revealing ones (power, per-device
  interrupts) need the strict diary permission. Admin tools get them; ordinary
  programs and the toolbox don't until each program can be granted rights on
  its own."
- **"General counters"**: the firmware-interrupt (`MSR_SMI_COUNT`) reading of
  stage 1 of
  `issues/toyos-runs-the-machine-in-acpi-mode-and-interprets-its-aml.md`
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
| Log | `issues/logging-records-from-every-producer-and-a-kernel-that-waits-on-nobody.md`; the sinks half of `issues/redesign-the-log-subsystem.md` |
| Diary | `issues/the-diary-computes-no-lateness-and-records-no-slow-system-call.md` |
| Counters, sizes | this file |
| Profile | behind the diary's judgement; `issues/a-frozen-toyos-waits-for-a-hand-on-the-power-button.md` |
| Accounting | `issues/nothing-charges-kernel-memory-to-a-process.md`, `issues/a-processs-memory-is-a-byte-total-that-reads-zero-under-contention.md`, `issues/blocked-time-is-invisible-while-the-park-lasts.md` |
| Inspect | `issues/the-kernel-keeps-nothing-it-enumerates.md` |
| Post-mortem | step 1 of `issues/logging-records-from-every-producer-and-a-kernel-that-waits-on-nobody.md` |
| Symbols | `issues/the-kernel-still-parses-what-userland-writes.md` Move 3 |

## What is to be built, in order (the orchestrator's)

Lane A, what `issues/toyos-beats-linuxs-latency-on-the-t14.md` needs:
the diary's tid 0 and
`issues/the-nmi-entry-can-hand-the-lockup-sample-cs-for-rflags-and-no-test-reds.md`;
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
there to the bound Intel's SDM gives it where it gives one, as `counters_on_metal` holds the busy
clock to the rate MPERF counts at. Linux's reading of each counter is a goal compared in
evidence
(`issues/linuxs-readings-of-the-t14-and-the-tcg-model-lack-reads-owed-before-the-t14s-wipe.md`),
never a row's expected value.
