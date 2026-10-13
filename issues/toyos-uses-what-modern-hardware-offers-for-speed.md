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

- A wake to a busy CPU waits out a whole 10 ms quantum on both benches:
  `issues/a-wake-to-a-busy-cpu-waits-for-its-next-pass-and-no-idle-cpu-can-take-it.md`.
- An MWAIT doorbell: the IPI's send and delivery and the kick's handler are
  at most 14.7 to 14.9 % of an idle wake on the T14 and 22.4 to 22.6 % on the
  AMD bench (`issues/idle-is-hlt-so-every-wake-of-an-idle-cpu-is-an-ipi.md`).
- A TSC deadline: the re-arm is at most 11.7 to 12.1 % of a pass's pick and
  switch on the T14, and at the AMD bench's TSC floor
  (`issues/the-timer-is-a-lapic-count-re-armed-every-pass-never-a-tsc-deadline.md`).

**Moot.** PCID on the AMD bench: its CPUs read `pcid=0 invpcid=0`, so there
is nothing to enable.

**Unexplained, and the measurement each needs next.**

- **The T14's clock under load.** Over the `internet_download` row's
  download windows ToyOS's median effective clock reads 2.67 GHz
  (`wt/toyos-scout-clock` at `fa6a2a829`), and read 2.53 GHz in round 1 of
  PR #861, and what holds it there is unread. Ruled out: the power limits
  (5.65 W median against a 20 W PL1, no power, thermal or current limit bit
  logged), the HWP request (`0x80002a04`) and C1E promotion (allowed); the one
  limit bit logged is multi-core turbo. ToyOS's cores never go below C1/C1E,
  its package never leaves PC0, and its idle package draws 3.16 W, which is
  `issues/idle-cpus-enter-the-c-states-cst-names.md`'s to answer; that the
  idle state reaches HWP's clock choice is inference. Next: a T14 scout whose
  idle CPUs wait in a deep C-state, read over the same windows.
- **Cycles per MB.** ToyOS spends 6.09 ms of C0 time and 16.20 Mcyc per MB
  over the same windows, at an IPC of 0.94. Next: cycles and instructions per
  MB split by where they run, app, network stack and kernel.
- **Global-line contention** is bounded at 2 to 3 % of C0 on the T14 and not
  attributed; next, the HITM count attributed to its lines on both benches
  (`issues/per-cpu-scheduler-and-pmm-state-shares-lines-every-cpu-writes.md`).
- **The wake storm's per-waiter gap** between the benches; next, its split
  (`issues/a-wake-storm-costs-the-t14-twice-the-amd-benchs-time-per-waiter-and-nothing-says-why.md`).

The CPU cap is PR #865's (`wt/toyos-cpus`), a design round of its own.

**Exit**: every issue below is closed. They are listed in the order they are
taken: the instruments, then what is measured, then the design rounds not
yet measured.
`issues/user-programs-use-avx-under-xsave.md` is PR #859's, built ahead of
that order.

- `issues/no-test-can-hold-a-thread-on-a-named-cpu.md`
- `issues/no-program-measures-toyos-against-linux-on-one-machine.md`
- `issues/a-wake-to-a-busy-cpu-waits-for-its-next-pass-and-no-idle-cpu-can-take-it.md`
- `issues/per-cpu-scheduler-and-pmm-state-shares-lines-every-cpu-writes.md`
- `issues/a-wake-storm-costs-the-t14-twice-the-amd-benchs-time-per-waiter-and-nothing-says-why.md`
- `issues/idle-is-hlt-so-every-wake-of-an-idle-cpu-is-an-ipi.md`
- `issues/the-timer-is-a-lapic-count-re-armed-every-pass-never-a-tsc-deadline.md`
- `issues/user-programs-use-avx-under-xsave.md`
- `issues/no-gate-decodes-kernel-elfs-instructions.md`
- `issues/kernel-forward-copies-and-fills-are-one-rep-movsb-or-stosb-on-every-cpu.md`
- `issues/the-direct-map-uses-1-gib-leaves-where-the-cpu-has-them.md`
- `issues/a-tlb-shootdown-reaches-only-the-cpus-that-ran-the-space.md`
- `issues/fsgsbase-is-taken.md`
- `issues/idle-cpus-enter-the-c-states-cst-names.md`
- `issues/diskserver-drives-nvme-apst.md`
- `issues/every-syscall-takes-the-process-table-lock-twice-to-count-itself.md`
- `issues/a-thread-resuming-on-the-cpu-that-holds-its-state-restores-it-again.md`
