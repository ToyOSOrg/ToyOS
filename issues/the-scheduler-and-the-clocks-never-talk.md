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
Filed so the placement track (see `placement-has-no-migration-cost-or-wake-affinity.md`)
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

## Job-fair scheduling (owner decision J1, 2026-10-21; stage 3)

Each CPU runs EEVDF over the job groups it holds, and EEVDF again over each
group's threads. A group's weight is its job's weight times its share of the
job's runnable threads, so a helper thread buys no entitlement. Placement
balances summed group weights and steals to idle CPUs. There is no cross-CPU
service accounting: a job receives, on each CPU running ordinary work, its
group weight over that CPU's load, and its aggregate share is as close to its
entitlement as placement makes it. Prompt interaction, bounded realtime (J2),
ordinary progress and job creation authority are kept.

The host model (`kernel/pure/sched/job_sched.rs`, PR #956) measures the
approximation as persistent rate gaps decided by placement: m+1 single-thread
jobs leave 0.30, 0.39 and 0.44 CPUs between jobs on four CPUs, the T14 and the
AMD shapes; a placement whose loads equal a fair one's, {A,A} {A,B} {B} {B},
holds 0.5 CPUs on four CPUs and 2 CPUs on AMD. While a runnable thread waits,
the service it is entitled to on its CPU stays within 0.48 of
`slice + 2·lag_bound + 2·slice` in every scenario run; in time that wait
scales with crowding. These are model figures for the scenarios that ran, not
hardware latency guarantees. Stage 3 validates the kernel implementation on
the supported machines.

Stage 3 is in the kernel. A job is a kernel object (`kernel/pure/sched/job.rs`,
`kernel/src/object/job.rs`) a process's threads and its children share unless a
spawn endows another under `JOB_LABEL`, and a launch carries its caller's; only a system capability's `JOBS` right
makes one, which the supervisor makes each service with and the compositor
each program it starts. Each CPU
runs the two EEVDF levels (`kernel/pure/sched/queue.rs`) over a deadline-ordered
tree augmented with each subtree's least virtual time, and decides again at a
request's end, at an earlier-due group's eligibility, and at a wake due first —
the last two once the running thread has run `MIN_SLICE_NS`. Every change to a
CPU's queues is made in that CPU's own pass, and every pass ends by arming the
timer for the earliest of these, so a remote steal re-arms it as a wake does.
Without the eligibility decision a job whose request is due first but carries a
little debt waits out every longer request ahead of it: in the model's desktop
mix beside a build the compositor then missed frames, finishing one 9.4 ms after
its release; with it, none on the T14 and AMD shapes. Busy CPUs balance summed
group weight, one request per CPU per `BALANCE_NS`. A voluntary yield ends the
yielder's request at both levels.

Stage 3 leaves three things. Explicit latency requests have no syscall: the
queue takes one request size for every thread until a first caller (the
compositor's or the audio worker's) needs a shorter one. `sshserver` starts its
sessions as its own helpers, since it has no ToyOS dependency to make a job
with. std and libc start a child in a new job only by `CommandExt::endow` under
`JOB_LABEL`.
