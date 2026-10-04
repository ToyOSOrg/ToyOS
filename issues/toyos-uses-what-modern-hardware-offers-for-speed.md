---
status: open
kind: track
opened: 2026-09-29
---

# ToyOS uses what modern hardware offers for speed

ToyOS takes each hardware feature that makes it faster on every CPU that
enumerates it; a CPU without one runs the plain path, or is refused by name
where the stage says so. The proving machines are the T14 and
AMD EPYC KVM guests, and a stage's correctness test runs on each that has its
feature. A figure is the T14's, since the EPYC guests are shared CI runners:
the median of 11 runs with its spread, taken by the program of
`issues/no-program-measures-toyos-against-linux-on-one-machine.md`.
A stage is done at or past that program's Linux figure, or with the gap filed
as a defect carrying both; a feature Linux has no counterpart for lands only
if it beats ToyOS without it. What no proving machine is known to offer is
`issues/features-no-proving-machine-is-known-to-offer.md`.

**Exit**: every issue below is closed, in the order listed.

- `issues/no-test-can-hold-a-thread-on-a-named-cpu.md`
- `issues/no-program-measures-toyos-against-linux-on-one-machine.md`
- `issues/user-programs-use-avx-under-xsave.md`
- `issues/no-gate-decodes-kernel-elfs-instructions.md`
- `issues/kernel-forward-copies-and-fills-are-one-rep-movsb-or-stosb-on-every-cpu.md`
- `issues/the-direct-map-uses-1-gib-leaves-where-the-cpu-has-them.md`
- `issues/a-tlb-shootdown-reaches-only-the-cpus-that-ran-the-space.md`
- `issues/fsgsbase-is-measured-then-taken-or-rejected.md`
- `issues/idle-cpus-enter-the-c-states-cst-names.md`
- `issues/diskserver-drives-nvme-apst.md`
