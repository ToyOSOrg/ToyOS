---
status: open
kind: track
opened: 2026-09-29
---

# ToyOS uses what modern hardware offers for speed

ToyOS takes each hardware feature that makes it faster on every CPU that
enumerates it; a CPU without one runs the plain path, or is refused by name
where the stage says so. Test machines are where this is measured, never what
it is designed for: a design is chosen for the modern hardware ToyOS targets,
and a machine that lacks a feature changes nothing but where it is proved.

The proving machines are the T14, the AMD bench (a Ryzen 7 4800U, family 17h
model 60h) and AMD EPYC KVM guests, and a stage's correctness test runs on
each that has its feature. A figure is the T14's, and the AMD bench's too
where the stage's path differs there, since the EPYC guests are shared CI
runners: the median of 11 runs with its spread, taken by the program of
`issues/no-program-measures-toyos-against-linux-on-one-machine.md`.
A stage is done at or past that program's Linux figure, or with the gap filed
as a defect carrying both; a feature Linux has no counterpart for lands only
if it beats ToyOS without it. What no proving machine is known to offer is
`issues/features-no-proving-machine-is-known-to-offer.md`.

**Measurement first.** Each design round starts from a posted measurement,
never a guess, judged by a kill condition fixed before the run. Three scouts
on `main`'s `177e72a9e` have read both benches, or the T14 against Ubuntu:
https://github.com/ToyOSOrg/ToyOS/pull/866#issuecomment-6119438569 (the
syscall and wake latency),
https://github.com/ToyOSOrg/ToyOS/pull/866#issuecomment-6119443198 (the TLB
and the idle wake) and
https://github.com/ToyOSOrg/ToyOS/pull/866#issuecomment-6119513916 (the clock
and contention).

**Measured, and worth building.** Each issue carries its reading.

- The syscall's FP save and the `lock` prefixes it takes are 34.5 % of a null
  syscall on the T14 and 43.4 % on the AMD bench:
  `issues/every-syscall-saves-the-fp-state-and-every-preemption-disable-is-locked.md`.
- A wake to a busy CPU waits out a whole 10 ms quantum on both benches, p99
  68 to 105 times a 100 µs bar:
  `issues/a-wake-to-a-busy-cpu-waits-for-its-next-pass-and-no-idle-cpu-can-take-it.md`.

**Killed.**

- `issues/an-mwait-doorbell-for-idle-cpus-is-not-worth-building.md`: the IPI
  is under a fifth of an idle wake on both benches.
- `issues/a-tsc-deadline-timer-is-not-worth-building-for-speed.md`: the
  re-arm is about 4 % of an idle wake on the T14 and below the AMD bench's
  TSC resolution.
- PCID on the AMD bench: its CPUs read `pcid=0 invpcid=0`, so there is
  nothing to enable.

**Unexplained, and the measurement each needs next.**

- **The T14's clock against Ubuntu's.** Over the `internet_download` row's
  download windows the medians read 2.67 against 3.08 GHz effective
  (`wt/toyos-scout-clock` at `fa6a2a829`). Ruled out: the power limits
  (5.65 W median against a 20 W PL1, no power, thermal or current limit
  bit logged), the HWP request (both `0x80002a04`) and C1E promotion (both
  allow it). The one limit bit ToyOS logged, multi-core turbo, Ubuntu logged
  in every window too. What differs: ToyOS's cores never go below C1/C1E and
  its package never leaves PC0, where Ubuntu's cores spend 57 to 79 % of a
  run in C7; and ToyOS's idle package draws 3.16 W against Ubuntu's 1.18 to
  1.57 W, which is `issues/idle-cpus-enter-the-c-states-cst-names.md`'s to
  answer. That the idle state reaches HWP's clock choice is inference, and
  the gap is not stable across sessions: round 1 of PR #861, `main`'s
  column, read ToyOS at 2.53 against Ubuntu's 2.03 GHz. Next: a T14 scout
  whose idle CPUs wait in a deep C-state, read against the same windows.
- **Cycles per MB.** ToyOS spends 1.98 times Ubuntu's C0 time per MB (6.09
  against 3.08 ms); the clock is about 1.15 of that and cycles per MB about
  1.58 (16.20 against 10.22 Mcyc), from about 1.17 times the instructions at
  an IPC of 0.94 against about 1.26 (Ubuntu's instructions derived from
  turbostat; ratios of medians). Next: cycles and instructions per MB split
  by where they run, app, network stack and kernel, on both systems.
- **The syscall's `lock` saving** exceeds what its prefixes cost in Ring 3;
  next, the prefix stripped one site at a time (its issue).
- **Global-line contention** is bounded at 2 to 3 % of C0 on the T14 and not
  attributed; next, the HITM count attributed to its lines on both benches
  (`issues/per-cpu-scheduler-and-pmm-state-shares-lines-every-cpu-writes.md`).
- **The wake storm's per-waiter gap** between the benches; next, its split
  (`issues/a-wake-storm-costs-the-t14-twice-the-amd-benchs-time-per-waiter-and-nothing-says-why.md`).
- **Global pages.** CR4.PGE is clear on both benches, so where there is no
  PCID every process switch drops the kernel's translations. On the AMD
  bench, which has none, a user ping-pong takes 80.7 full page walks per round trip with no
  user working set and 88.6 at four pages, which prices at about 7 % of the
  round trip (derived, user and kernel walks together, so a ceiling on what
  global pages save). On the T14, PCID saves 3.4 % of the same round trip at
  four pages (`wt/toyos-scout-tlb` at `d4c2f8ebe`). The decision and its
  sequencing are `issues/page-global-is-a-decision-nobody-has-made.md`'s;
  next, a PGE twin on the AMD bench.

The CPU cap is PR #865's (`wt/toyos-cpus`), a design round of its own.

**Exit**: every issue below is closed. They are listed in the order they are
taken: the instruments, then what is measured and worth building, then what
waits on its next measurement, then the design rounds not yet measured.
`issues/user-programs-use-avx-under-xsave.md` is PR #859's, built ahead of
that order.

- `issues/no-test-can-hold-a-thread-on-a-named-cpu.md`
- `issues/no-program-measures-toyos-against-linux-on-one-machine.md`
- `issues/every-syscall-saves-the-fp-state-and-every-preemption-disable-is-locked.md`
- `issues/a-wake-to-a-busy-cpu-waits-for-its-next-pass-and-no-idle-cpu-can-take-it.md`
- `issues/per-cpu-scheduler-and-pmm-state-shares-lines-every-cpu-writes.md`
- `issues/a-wake-storm-costs-the-t14-twice-the-amd-benchs-time-per-waiter-and-nothing-says-why.md`
- `issues/user-programs-use-avx-under-xsave.md`
- `issues/no-gate-decodes-kernel-elfs-instructions.md`
- `issues/kernel-forward-copies-and-fills-are-one-rep-movsb-or-stosb-on-every-cpu.md`
- `issues/the-direct-map-uses-1-gib-leaves-where-the-cpu-has-them.md`
- `issues/a-tlb-shootdown-reaches-only-the-cpus-that-ran-the-space.md`
- `issues/fsgsbase-is-measured-then-taken-or-rejected.md`
- `issues/idle-cpus-enter-the-c-states-cst-names.md`
- `issues/diskserver-drives-nvme-apst.md`
