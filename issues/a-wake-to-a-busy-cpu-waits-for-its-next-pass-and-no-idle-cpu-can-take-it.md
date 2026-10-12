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
both machines, while the wakes to a sleeping one ran within 2.6 µs, and the
pair fell from about 181,600 (T14) and 205,200 (AMD bench) rounds a second
with no spinners to 100. The kill condition fixed before the run, the change
dropped if the busy-target p99 is under about 100 µs with 7 and with 8
spinners on both benches, did not fire: it reads 68 to 105 times that.

**Exit**: a metal row on the T14 and the AMD bench that wakes a task onto a
CPU running a CPU-bound thread, with 7 and with 8 such threads on 8 CPUs, and
holds its post-to-dispatch p99 under 100 µs.
