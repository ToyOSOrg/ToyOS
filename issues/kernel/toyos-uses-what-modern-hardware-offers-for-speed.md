---
status: open
kind: track
opened: 2026-09-29
---

# ToyOS uses what modern hardware offers for speed

ToyOS takes each hardware feature that makes it faster on every CPU that
enumerates it; a CPU without one runs the plain path, or is refused by name
where the stage says so. The proving machines are the T14 and the nightly's
AMD EPYC KVM guests, and a stage's correctness test runs on each that has its
feature. A figure is the T14's, since the EPYC guests are shared CI runners:
the median of 11 runs with its spread, taken by the program of
`issues/hardware/no-program-measures-toyos-against-linux-on-one-machine.md`.
A stage is done at or past that program's Linux figure, or with the gap filed
as a defect carrying both; a feature Linux has no counterpart for lands only
if it beats ToyOS without it. What no proving machine is known to offer is
`issues/hardware/features-no-proving-machine-is-known-to-offer.md`.

**Exit**: every issue below is closed, in the order listed.

- `issues/kernel/no-test-can-hold-a-thread-on-a-named-cpu.md`
- `issues/hardware/no-program-measures-toyos-against-linux-on-one-machine.md`
- `issues/kernel/user-programs-use-avx-under-xsave.md`
- `issues/kernel/kernel-copies-use-rep-movsb-where-the-cpu-has-erms.md`
- `issues/kernel/the-direct-map-uses-1-gib-leaves-where-the-cpu-has-them.md`
- `issues/kernel/a-tlb-shootdown-reaches-only-the-cpus-that-ran-the-space.md`
- `issues/kernel/fsgsbase-is-measured-then-taken-or-rejected.md`
- `issues/kernel/idle-cpus-enter-the-c-states-cst-names.md`
- `issues/hardware/blockd-drives-nvme-apst-and-the-host-memory-buffer.md`
