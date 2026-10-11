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

**Measurement first.** Each design round below starts from a posted
measurement, never a guess. In flight:

- `wt/toyos-scout-clock`: the clock gap, and the lock contention of
  `issues/per-cpu-scheduler-and-pmm-state-shares-lines-every-cpu-writes.md`.
  In round 3 of PR #861 on the T14, ToyOS ran at 1.91 GHz effective against
  Ubuntu's 3.12, spent 5.45 against 3.03 ms of C0 per MB, and its idle window
  burnt 36 to 59 Mcyc/s against Ubuntu's 13.1 (that PR's body, "Round 3 on
  the T14").
- `wt/toyos-scout-sys`: the syscall split of
  `issues/every-syscall-saves-the-fp-state-and-takes-two-locked-instructions.md`,
  and the wake latency of
  `issues/a-wake-to-a-busy-cpu-waits-for-its-next-pass-and-no-idle-cpu-can-take-it.md`.

Next: the AMD bench's TLB cost across a cross-process ping-pong, which prices
`issues/pcid-waits-on-invpcid-so-the-amd-bench-flushes-its-tlb-on-every-process-switch.md`
and may first need ToyOS to read an AMD performance counter; and the T14's
idle-exit split, which decides
`issues/idle-is-hlt-so-every-wake-of-an-idle-cpu-is-an-ipi.md` and times the
re-arm of `issues/the-timer-is-a-lapic-count-re-armed-every-pass-never-a-tsc-deadline.md`.

The CPU cap is PR #865's (`wt/toyos-cpus`), a design round of its own.

**Exit**: every issue below is closed. They are listed in the order they are
taken: the instruments, then each issue whose measurement is in flight, then
those whose measurement is next, then the design rounds already measured or
not yet begun. `issues/user-programs-use-avx-under-xsave.md` is PR #859's,
built ahead of that order.

- `issues/no-test-can-hold-a-thread-on-a-named-cpu.md`
- `issues/no-program-measures-toyos-against-linux-on-one-machine.md`
- `issues/a-wake-storm-costs-the-t14-twice-the-amd-benchs-time-per-waiter-and-nothing-says-why.md`
- `issues/every-syscall-saves-the-fp-state-and-takes-two-locked-instructions.md`
- `issues/a-wake-to-a-busy-cpu-waits-for-its-next-pass-and-no-idle-cpu-can-take-it.md`
- `issues/per-cpu-scheduler-and-pmm-state-shares-lines-every-cpu-writes.md`
- `issues/pcid-waits-on-invpcid-so-the-amd-bench-flushes-its-tlb-on-every-process-switch.md`
- `issues/idle-is-hlt-so-every-wake-of-an-idle-cpu-is-an-ipi.md`
- `issues/the-timer-is-a-lapic-count-re-armed-every-pass-never-a-tsc-deadline.md`
- `issues/physical-memory-past-64-gib-panics-the-boot.md`
- `issues/user-programs-use-avx-under-xsave.md`
- `issues/no-gate-decodes-kernel-elfs-instructions.md`
- `issues/kernel-forward-copies-and-fills-are-one-rep-movsb-or-stosb-on-every-cpu.md`
- `issues/the-direct-map-uses-1-gib-leaves-where-the-cpu-has-them.md`
- `issues/a-tlb-shootdown-reaches-only-the-cpus-that-ran-the-space.md`
- `issues/fsgsbase-is-measured-then-taken-or-rejected.md`
- `issues/idle-cpus-enter-the-c-states-cst-names.md`
- `issues/diskserver-drives-nvme-apst.md`
