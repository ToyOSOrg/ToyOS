# Scheduling and power

## Jobs share the CPU

```text
   CPU n:  EEVDF over job groups ──> EEVDF over that group's threads
           weight(group) = weight(job) × runnable here / runnable in job
```

- CPU time is shared by jobs, not threads. A process's and its helpers'
  threads share one job, so helpers buy no extra share; authorised launchers
  create independent jobs.
- Nothing reserves CPU time: no program is promised a budget, and nothing is
  admitted against one.
- Every job has equal weight, across users too. Window focus changes
  responsiveness, not sustained entitlement.
- Each CPU runs EEVDF over the job groups it holds, and again among each
  group's threads. A thread that sleeps and runs briefly gets the next slice
  without more total time, so an editor answers a key while a compiler keeps
  its share of the hour.
- A thread's request, the service it asks for before its CPU decides again,
  follows the length of its recent runs between sleeps, never below a minimum
  slice and never above the full one; a group asks for its shortest thread's.
  A shorter request is due sooner, so it changes the order in which threads
  run and never their share of the CPU.
- Placement balances group weight across CPUs and steals work for idle CPUs.
  There is no cross-CPU service accounting; aggregate fairness is as close as
  placement makes it.

## Real time

- Audio runs at bounded real-time priority, independent of window focus.
- Each CPU has an aggregate real-time allowance, replenished each interval,
  that protects ordinary progress. Once it is exhausted, that CPU's real-time
  workers run only when no ordinary work wants the CPU, until it refills.
- New streams are accepted best-effort; genuine overload may cause dropouts, and a late
  client contributes silence without holding back the others.

## Power

- Useful work runs promptly; no power target holds it back.
- Idle cores sleep within their wake-latency constraints. Placement and
  idle-state selection account for wake cost and known upcoming work.
- Ordinary load never starves audio.
- The kernel declares the CPU's performance and power bounds as part of its
  one CPU declaration and reports them back; the CPU chooses frequencies within
  them.
