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

Idle-state awareness has its first half: a wake forward prefers, among
equally near free cores, the one whose state wakes quickest
(`CpuHandles::idle_near`, with the exit each idle CPU publishes from
`kernel/src/idle.rs`), and a spawn, among free cores' sleepers with equal
queues, does the same (`CpuHandles::place`). A latency-sensitive wake preferring a shallow core
over a nearer deep one, and a throughput task choosing to pay a deep one's
exit, wait for the explicit latency requests below, whose caller says which
it is. The kernel chooses no P-state: it declares HWP's bounds
(`kernel/src/arch/x86_64/control_regs.rs`), and the CPU chooses within them.
The placement track (`placement-has-no-migration-cost-or-wake-affinity.md`)
takes the published exit as its idle-state input.

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

Stage 3 is in the kernel (`kernel/pure/sched/queue.rs`, `kernel/pure/sched/job.rs`,
`kernel/src/object/job.rs`). Its remaining work:

- **Explicit latency requests.** The queue gives every thread one request
  size, and no syscall asks for a shorter one. Deferred until a first caller
  needs it (orchestrator, 2026-10-21): stage 4's audio workers are the
  expected one. Exit: that caller's stage adds a bounded request that changes
  order, not weight, and its test shows the caller meeting its deadline beside
  saturating jobs.
- **sshserver sessions.** `sshserver` starts every session as one of its own
  helpers, so all remote logins share its job: it has no ToyOS dependency
  through which to make one. Exit: sshserver holds `jobs` and starts each session in a job
  of its own, through std's endowment of `JOB_LABEL` or the std/libc path
  below.
- **A new job from std and libc.** Their spawn reaches a new job only through
  `CommandExt::endow(JOB_LABEL, …)`, and libc has no such call. This waits
  for the fd-table work that rewrites spawn. Exit: std and libc each take a
  job for a child through their ordinary spawn interface, which is then the
  route the bullet above uses.
