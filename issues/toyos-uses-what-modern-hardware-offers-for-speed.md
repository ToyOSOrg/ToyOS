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

**Moot.** PCID on the AMD bench: its CPUs read `pcid=0 invpcid=0`, so there
is nothing to enable.

**Unexplained, and the measurement each needs next.**

- **The T14's one-thread clock is the three-active-core turbo bin.** Its
  `MSR_TURBO_RATIO_LIMIT` reads `0x2626262626262a2a`: ratio 42 with one or
  two active cores, 38 with three or more (#866's Ubuntu turbostat dump,
  https://github.com/ToyOSOrg/ToyOS/pull/866#issuecomment-6119512404).
  `cpu.clock`'s maximum is 3789 MHz under ToyOS in every osbench group and
  4190 MHz under Linux in every group whose probes leave its idle clock,
  1.106 apart where 42 and 38 are 1.105, and the
  one-thread `crunch` rows run 1.09 to 1.10 times slower (PR #1066's
  evidence). Ruled out: the HWP request (maximum 42 under both systems),
  turbo (engaged, `IA32_MISC_ENABLE[38]` clear), the power limits (no power,
  thermal or current limit bit logged) and C1E promotion (the same under
  both). The limit bit logged is multi-core turbo. What differs is idle:
  ToyOS's idle cores wait in `hlt`, C1/C1E, and Linux's in C7. Intel's
  datasheet counts the cores "operating in the C0 state" (631121-012
  §2.4.6.3), so that a core in C1 counts as active is inference until a
  Linux run limited to C1 and PR #1066's deep-idle scout read it. The fix
  is `issues/idle-cpus-enter-the-c-states-cst-names.md`'s: no register ToyOS
  declares holds the clock. The AMD bench shows no such gap: its
  `cpu.clock` p50 is 4289 MHz under ToyOS and at most 4033 under Linux.
- **Global-line contention** is bounded at 2 to 3 % of C0 on the T14 and not
  attributed; next, the HITM count attributed to its lines on both benches
  (`issues/per-cpu-scheduler-and-pmm-state-shares-lines-every-cpu-writes.md`).
- **The wake storm's per-waiter gap** between the benches; next, its split
  (`issues/a-wake-storm-costs-the-t14-twice-the-amd-benchs-time-per-waiter-and-nothing-says-why.md`).

The CPU cap is PR #865's (`wt/toyos-cpus`), a design round of its own.

**Exit**: every issue below is closed. They are listed in the order they are
taken: the instruments, then what is measured, then the design rounds not
yet measured.

- `issues/no-test-can-hold-a-thread-on-a-named-cpu.md`
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
