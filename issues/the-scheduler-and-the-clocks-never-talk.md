---
status: open
kind: track
opened: 2026-08-16
---

# The scheduler and the clocks never talk

## Combined scheduler, audio and idle direction

Use per-CPU EEVDF job groups and bounded realtime audio. No EDF/CBS admission
or per-stream CPU reservation. Focus changes responsiveness, not sustained
shares; equal-weight jobs compete across users without an outer user hierarchy.
Independent jobs need launch authority; helpers share their existing job.

Accept new audio streams best-effort. Genuine audio overload may cause dropouts;
a client missing a block contributes silence without delaying other clients.
Grant realtime authority only to the relevant workers, not every thread in an
application. Preallocate realtime work and avoid blocking/allocation in callbacks.
Owner-keyed abuse handling remains distinct from legitimate aggregate overload.

Idle selection uses predicted idle duration, known next wake and permitted exit
latency. Placement considers wake cost without sacrificing ordinary progress.
Run batch work promptly with no power target; let idle cores sleep. Detailed
frequency optimization is later work. Scheduling, timer programming and the idle
entry recheck must prevent lost wakeups; firmware latency is not a hard guarantee.

Reconcile the old CPU/idle drafts with this direction. Complete kernel scheduling,
then realtime authorization/limits and the soundserver client path, then idle-state
integration. Validate accounting on the host and actual wake/audio behavior on
metal in focused, batched runs. Do not require a new latency-hint API before its
first caller. Existing model results below do not establish hardware performance.

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
