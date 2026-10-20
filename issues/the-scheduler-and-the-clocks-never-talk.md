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
(`issues/toyos-runs-the-machine-in-acpi-mode-and-interprets-its-aml.md`),
because the kernel reads no C-state on metal and chooses no P-state: it
declares HWP's bounds (`kernel/src/arch/x86_64/control_regs.rs`), and the CPU
chooses within them.
Filed so the placement track (see `placement-is-blind-to-caches-and-topology.md`)
leaves room for an idle-state input when it designs wake placement.

## Realtime exhaustion (owner decision J2, 2026-10-20; stage 4)

Once a CPU's realtime allowance is exhausted, the affected audio workers may
run only when no ordinary work can use that CPU, until the allowance refills.
They do not compete for the protected ordinary time through their job's fair
share. More dropouts during genuine audio overload are accepted. This replaces
the plan's ordinary-priority fallback, which the stage-1 host model
(`kernel/pure/sched/job_sched.rs`, PR #956) measured giving never-revoked
spinners 0.8 of the 1.2 CPUs a 70% ceiling leaves on four CPUs, and some
intervals no ordinary service at all. Model figures are evidence for the
scenarios they ran, not hardware guarantees or general bounds.

The per-CPU ceiling does not replace the owner-level abuse rules. The model's
runaway record counts realtime per thread since it last blocked, so a stream
reopened on the same thread keeps its count, but a spinner that blocks for
1 µs every 40 ms, or a worker replaced by a fresh granted thread every 40 ms,
is never struck
(`checkpoint::counterexample_runaway_detection_misses_brief_blocks_and_replaced_threads`).
Stage 4 owes an owner-keyed rule that survives thread replacement, brief
blocking and stream reopening alike, without calling aggregate overload or a
legitimate multi-worker client abuse.

## Open: the cross-CPU fairness candidate repays a dip below a job's share

The host model's cross-CPU fairness (`Params::cross_cpu_lag`, PR #956, awaiting
the owner's decision J1) orders each CPU's jobs by their normalized service.
A job whose runnable count dips below its fluid share without reaching zero
shows a deficit against the jobs beside it, and that ordering repays it after
the dip.

- **Evidence.** Four threads running 1 ms and sleeping 0.1 ms beside eight hogs
  on four identical CPUs: the job receives 2.000 CPUs where its fluid share is
  1.975, an error of 205 ms after 8 s that grows linearly. The asserted sweep
  (`checkpoint::OPEN`) holds it to 250 ms, which is specific to its 10 s run.
  The size depends on how the dips coincide: with 1 µs between the sleepers'
  sleeps it is 41 ms; on the T14 and AMD shapes 15 and 41 ms. Logs:
  https://github.com/ToyOSOrg/evidence/tree/7395cd9ba5690abdc571c98c48d99e6a5d117e3a/round3-investigation
- **Owner.** This track, with J1: the defect belongs to the candidate, not to
  the plan's per-CPU arithmetic.
- **Exit.** A rule that tells a demand dip from a deficit owed, shown by this
  scenario under the sweep's normal ceiling at every window and no regression
  elsewhere; or J1 choosing a different cross-CPU mechanism. A per-job rule,
  "a job with no waiting thread accrues no deficit", forgave genuine deficits
  in both forms tried.

The same ordering bounds each job's share but not each thread's wait: a
thread of a job served on other CPUs, beside a job behind it, waits until
placement moves it (`tests::a_thread_of_a_job_served_elsewhere_waits_until_placement_moves_it`:
under 8 ms of a second, no migration).
