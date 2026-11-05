# Scheduling and power

## Jobs share the CPU

CPU time is shared by jobs, not threads. A process's threads and its helpers'
threads belong to the same job, so starting helpers buys no extra share;
authorised launchers create independent jobs. Every job has equal weight,
across users too: there is no outer per-user hierarchy, and window focus changes
responsiveness, not sustained entitlement.

Each CPU runs EEVDF over the job groups it holds, and EEVDF again among each
group's threads. A group's weight is its job's weight times its share of the
job's runnable threads. Placement balances group weight across CPUs and steals
work for idle CPUs. There is no cross-CPU service accounting: aggregate fairness
is approximate, as close as placement makes it.

```text
   CPU n:  EEVDF over job groups ──> EEVDF over that group's threads
           weight(group) = weight(job) × runnable here / runnable in job
```

EEVDF lets a thread that sleeps and runs briefly get the next slice without
receiving more total time, so an editor answers a key while a compiler keeps its
share of the hour.

## Real time

Audio runs at bounded real-time priority, independent of window focus. Each CPU
has an aggregate real-time allowance, replenished each interval, that protects
ordinary progress. Once it is exhausted, the real-time workers on that CPU run
only when no ordinary work wants the CPU, until the allowance refills. There is
no per-stream reservation and no deadline-admission machinery. New streams are
accepted best-effort; genuine audio overload may cause dropouts, and a late
client contributes silence without holding back the others. Real-time
scheduling is a right on the system capability.

## Power

Useful work runs promptly; there is no power target that holds it back. Idle
cores sleep within their wake-latency constraints. Placement and idle-state
selection account for wake cost and known upcoming work: a latency-sensitive
wake prefers a core in a shallow sleep. Ordinary load must never starve audio.

The kernel owns the CPU's performance state: it declares the hardware's
performance and power bounds as part of its one CPU declaration and reports
them back, and the CPU chooses frequencies within them.
