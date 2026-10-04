---
status: open
kind: track
opened: 2026-10-03
---

# ToyOS beats Linux's latency on the T14

Ruled (owner, 2026-10-03): the bar of the latency work is Linux's reading on
the same T14, and its order is the three steps below, with stage 4 of
`issues/design-debt/toyos-has-its-own-network-stack.md` running beside them.

1. **The firmware's interrupt**: stage 1 of
   `issues/kernel/toyos-runs-the-machine-in-acpi-mode-and-interprets-its-aml.md`.
2. **Syscalls run with interrupts on**:
   `issues/kernel/syscall-preemption-is-incidental.md`.
3. **USB and storage leave the kernel**: usbd, step 10 of
   `issues/kernel/the-kernel-is-small-interrupts-post-and-threads-wait.md`.

**Linux's reading, the reference.** Ubuntu's `6.8.0-142-generic` on the same
machine, `rtla timerlat top -q -d 1m --dma-latency 0`, a 1 kHz timer on each
CPU (#649, comment 5961690204; its log whole in #682's comment 5968053374).
It samples: a window under 1 ms is seen only when a tick falls in it.

- The timer interrupt's longest lateness on a CPU: 3 to 99 µs idle and 16 to
  131 µs with every CPU spawning `/bin/true`, on seven CPUs. cpu4 read 2952
  and 2880 µs, one event a run, its cause not identified; a two-minute idle
  run after them read 4 to 53 µs on all eight (#681, comment 5962619169).
- A woken thread's longest lateness: 9 to 571 µs idle and 38 to 503 µs
  loaded on those seven; cpu4 2964 and 2917.
- No firmware interrupt on any CPU in 120.378 s, with `SCI_EN` set (#681,
  comments 5962619169 and 5962768253).

**Ruled** (owner, 2026-10-03): **"131 µs"**. ToyOS must beat Linux's worst
delay on the other seven CPUs, 131 µs, not cpu4's one-off 2.9 ms event.

**Exit**: each step's exit is met, in the file the step names, and on the T14,
with every CPU spawning a program that exits at once, as under Linux's
131 µs reading, the longest lateness of a 1 kHz timer's interrupt on each CPU reads under 131 µs,
and of the thread it wakes under Linux's loaded longest on those seven CPUs,
503 µs. Nothing takes that reading of ToyOS today:
`latency_wake` reads one thread's p99 and `mask_windows` each CPU's longest
masked windows. Two files owe it: step 2 of
`issues/diagnostics/the-diary-computes-no-lateness-and-records-no-slow-system-call.md`, a
reader that computes timer and thread lateness from the ring, and
`issues/hardware/no-program-measures-toyos-against-linux-on-one-machine.md`,
one program that reads lateness past a 1 ms timer under both systems.
