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
`issues/no-published-run-compares-toyos-with-linux-on-one-machine.md`.
A stage that its hardware's manufacturers or the published state of the art
back follows and cites them, and its figure is reported and never blocks it: a
slowdown is filed as a defect carrying both figures and investigated. A stage nothing published covers is
reviewed on its correctness and simplicity, its figure reported and deciding
nothing by itself unless speed is the stage's only purpose. What no proving
machine is known to offer is
`issues/features-no-proving-machine-is-known-to-offer.md`.

**Measurement first.** Each design round starts from a posted measurement,
never a guess. Three scouts on `main`'s `177e72a9e` have read both benches:
https://github.com/ToyOSOrg/ToyOS/pull/866#issuecomment-6119438569 (the
syscall and wake latency),
https://github.com/ToyOSOrg/ToyOS/pull/866#issuecomment-6119443198 (the TLB
and the idle wake) and
https://github.com/ToyOSOrg/ToyOS/pull/866#issuecomment-6119513916 (the clock
and contention).

**Measured.** Each issue carries its reading.

- A wake to a busy CPU beside idle ones is forwarded since #874, p99 5 µs
  on the T14 and 25 µs on the AMD bench; beside seven spinners no row holds it
  to 100 µs: `issues/no-row-holds-a-wake-beside-seven-spinners-to-100-us.md`.
- An MWAIT doorbell: the IPI's send and delivery and the kick's handler are
  at most 14.7 to 14.9 % of an idle wake on the T14 and 22.4 to 22.6 % on the
  AMD bench (`issues/idle-is-hlt-so-every-wake-of-an-idle-cpu-is-an-ipi.md`).
- A TSC deadline: before #1061 cut the arm to one register write, the
  re-arm was at most 11.7 to 12.1 % of a pass's pick and switch on the T14,
  and at the AMD bench's TSC floor; it is to be read again
  (`issues/the-timer-is-a-lapic-count-re-armed-every-pass-never-a-tsc-deadline.md`).
- Deep idle: on the T14 `hlt` holds a one-thread load to the turbo ratio
  for three or more active cores, 38 of `MSR_TURBO_RATIO_LIMIT`'s
  `0x2626262626262a2a` against 42 for one or two (#866's dump,
  https://github.com/ToyOSOrg/ToyOS/pull/866#issuecomment-6119512404), the
  limit bit logged being multi-core turbo. HWP's request (maximum 42),
  turbo, the power limits and C1E promotion were ruled out. Under `hlt`,
  `cpu.clock` reads at most 3789 MHz in every osbench group. What deep idle
  reads is `issues/idle-cpus-enter-the-c-states-cst-names.md`'s. The AMD
  bench has no such gap: its one-thread `crunch.single` deflate, matmul and
  sha256 run at 0.98 to 1.02 of Linux's; parse, faster under ToyOS on both
  benches, is no clock reading.

**Moot.** PCID on the AMD bench: its CPUs read `pcid=0 invpcid=0`, so there
is nothing to enable.

**Unexplained, and the measurement each needs next.**

- **The T14's effective clock over the `internet_download` row's download
  windows**: its median is 2.67 GHz under ToyOS against 3.08 under Linux
  (`wt/toyos-scout-clock` at `fa6a2a829`), and read 2.53 GHz in round 1 of
  PR #861. The bin of 38 bounds it at 3.8 GHz and does not explain it. Next:
  the same windows read again once idle CPUs leave `hlt`.
- **Global-line contention** is bounded at 2 to 3 % of C0 on the T14 and not
  attributed; next, the HITM count attributed to its lines on both benches
  (`issues/per-cpu-scheduler-and-pmm-state-shares-lines-every-cpu-writes.md`).
- **The wake storm's per-waiter gap** between the benches; next, its split
  (`issues/a-wake-storm-costs-the-t14-twice-the-amd-benchs-time-per-waiter-and-nothing-says-why.md`).

The CPU cap is PR #865's (`wt/toyos-cpus`), a design round of its own.

**Exit**: every issue below is closed. They are listed in the order they are
taken: the instruments, then what is measured, then the design rounds not
yet measured.

- `issues/no-published-run-compares-toyos-with-linux-on-one-machine.md`
- `issues/no-row-holds-a-wake-beside-seven-spinners-to-100-us.md`
- `issues/per-cpu-scheduler-and-pmm-state-shares-lines-every-cpu-writes.md`
- `issues/a-wake-storm-costs-the-t14-twice-the-amd-benchs-time-per-waiter-and-nothing-says-why.md`
- `issues/idle-is-hlt-so-every-wake-of-an-idle-cpu-is-an-ipi.md`
- `issues/the-timer-is-a-lapic-count-re-armed-every-pass-never-a-tsc-deadline.md`
- `issues/no-gate-decodes-kernel-elfs-instructions.md`
- `issues/kernel-forward-copies-and-fills-are-one-rep-movsb-or-stosb-on-every-cpu.md`
- `issues/the-direct-map-uses-1-gib-leaves-where-the-cpu-has-them.md`
- `issues/a-tlb-shootdown-reaches-only-the-cpus-that-ran-the-space.md`
- `issues/fsgsbase-is-taken.md`
- `issues/idle-cpus-enter-the-c-states-cst-names.md`
- `issues/diskserver-drives-nvme-apst.md`
- `issues/syscall-cost-is-unread-since-a-syscall-counts-itself-on-its-thread.md`
- `issues/malloc-churn-across-eight-threads-fell-with-the-run-queue-arenas.md`
- `issues/a-thread-resuming-on-the-cpu-that-holds-its-state-restores-it-again.md`
