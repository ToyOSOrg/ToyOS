---
status: assigned
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
hands over from the run queue (`cpu.rs:1881-1891`). So a task woken onto a CPU
running a CPU-bound thread can wait up to a quantum while another CPU idles.
`issues/watch-window-spins-out-a-hold-its-poster-is-queued-behind.md` is one
symptom of it. The latency it adds has not been measured on any bench.

`wt/toyos-scout-sys` holds the measurement: wake-to-run latency on the T14
with the target CPU busy and another idle.

**Exit**: that latency, posted; and a T14 row in which a task woken onto a CPU
running a CPU-bound thread, while another CPU idles, runs before that thread's
quantum ends.
