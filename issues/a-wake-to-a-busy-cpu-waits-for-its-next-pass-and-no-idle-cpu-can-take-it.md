---
status: open
kind: defect
opened: 2026-10-12
---

# A wake to a busy CPU waits for its next pass, and no idle CPU can take it

A wake goes to the CPU the task parked on (`kernel/pure/sched/park.rs:109-128`).
An ordinary wake carries `Urgency::Normal` (`kernel/pure/sched/task.rs:71-76`),
and its doorbell sends an IPI only when the target sleeps
(`kernel/pure/sched/mailbox.rs:474-490`). A busy target drains the message at
its next pass: when its task blocks, or when the 10 ms quantum ends
(`kernel/pure/sched/fair.rs:16`, `kernel/src/scheduler.rs:400-420`).

Until then the woken task is in that CPU's mailbox, not its run queue, and an
idle CPU's steal probe reads only the run queue's fair band: the victim's
published surplus is `rq.fair_len()` (`kernel/pure/sched/cpu.rs:1668-1670`),
`best_victim` picks by it (`cpu.rs:2082-2088`), and `answer_steal_requests`
hands over from the run queue (`cpu.rs:1881-1891`).
`issues/watch-window-spins-out-a-hold-its-poster-is-queued-behind.md` is one
symptom of it.

**Measured** (`wt/toyos-scout-sys` at `fbb958b17`, `main`'s `177e72a9e` plus
the scout;
https://github.com/ToyOSOrg/ToyOS/pull/866#issuecomment-6119438569). A
ping-pong pair, with CPU-bound spinners beside it, timed from post to
dispatch on the kernel clock; quantiles are bucket upper bounds, accurate to
12.5 %:

| bench | spinners on 8 CPUs | target asleep, p99 | target busy, p50 / p99 / max |
|---|---|---|---|
| T14 | 7 | ≤2.30 µs | ≤10.49 ms / ≤10.49 ms / 10.00 ms |
| T14 | 8 | — | ≤3.41 ms / ≤6.82 ms / 6.69 ms |
| AMD bench | 7 | ≤2.56 µs | ≤10.49 ms / ≤10.49 ms / 10.00 ms |
| AMD bench | 8 | — | ≤2.62 ms / ≤7.86 ms / 7.55 ms |

With 7 spinners every wake to a busy target waited out a whole quantum on
both machines, while the slowest wake to a sleeping one took 2.16 µs (T14)
and 3.17 µs (AMD bench), and the pair fell from about 181,600 (T14) and
205,200 (AMD bench) rounds a second with no spinners to 100.

With 7 spinners this is the Overload-on-Wakeup bug of Lozi et al., "The Linux
Scheduler: a Decade of Wasted Cores" (EuroSys 2016, §3.3): a thread that was
asleep wakes on an overloaded core while other cores are idle. Its fix
wakes the thread on the core it last ran on if that core is idle, and
otherwise on the core idle longest, where one is; the paper applies it only
where the power policy keeps cores out of low-power states, since waking a
core from one costs power. With 8 spinners no core idles, and a wake onto a
CPU running a task is the fair band's pick to decide, which is the
vruntime/lag family (`kernel/pure/sched/fair.rs:1-3`) of EEVDF. There the
resource is "allocated in time quanta of size at most q. At the beginning of
each time quantum a client is selected", and "A new quantum is allocated to
the client which has the eligible request with the earliest virtual deadline"
(Stoica et al., "A Proportional Share Resource Allocation Algorithm for
Real-Time, Time-Shared Systems", RTSS 1996, the algorithm of Stoica and
Abdel-Wahab's TR-95-22), so a task woken inside a quantum is picked no
sooner than the next one's start. That arm follows it with a wake's q the protected
slice, 1 ms (`PROTECTED_RUN_NS`, https://github.com/ToyOSOrg/ToyOS/pull/874),
so its test is that a woken task runs within one protected slice, and its p99
against 100 µs is reported. With 7 spinners the 100 µs bound is the exit's
test that the stage works, not the judge of its design.

**Exit**: a metal row on the T14 and the AMD bench that wakes a task onto a
CPU running a CPU-bound thread, with 7 and with 8 such threads on 8 CPUs:
with 7 it holds the post-to-dispatch p99 under 100 µs, and with 8 it holds
that a woken task runs within one protected slice and reports its p99.
