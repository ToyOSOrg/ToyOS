---
status: open
kind: track
opened: 2026-08-16
---

# The scheduler and the clocks never talk

Queued track from the 2026-08-16 owner conversation on scheduler direction; the
furthest out of the set.

Two couplings every production scheduler has and ours lacks:

- **Frequency (DVFS)**: the scheduler knows every core's utilization and nobody
  else does, so it should feed whoever sets clock speeds (the schedutil
  arrangement) — including the race-to-idle question of whether finishing fast
  and sleeping beats pacing slow.
- **Idle-state awareness**: a deeply sleeping core takes real microseconds to
  wake. A latency-sensitive wake should prefer a shallow-idle core; a
  throughput task can pay to wake a deep one.

Both wait for an AML interpreter
(`issues/kernel/toyos-runs-the-machine-in-acpi-mode-and-interprets-its-aml.md`),
because the kernel reads no C-state on metal and chooses no P-state: it
declares HWP's bounds (`kernel/src/arch/x86_64/control_regs.rs`), and the CPU
chooses within them.
Filed so the placement track (see `placement-is-blind-to-caches-and-topology.md`)
leaves room for an idle-state input when it designs wake placement.
