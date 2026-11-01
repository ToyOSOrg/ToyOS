---
status: open
kind: defect
opened: 2026-10-12
---

# An ordinary wake posted on its own CPU waits for that CPU's next pass

`park::deliver` (`kernel/pure/sched/park.rs`) posts an ordinary wake
(`Urgency::Normal`) whose target CPU is the one the poster runs on without
ringing that CPU's doorbell: the CPU's own next pass drains it. A device
interrupt that posts a wake sets `need_resched` before its EOI, so the CPU it
interrupted runs that pass on the way out. A thread that wakes a task parked
on its own CPU and keeps running is what is left: the woken task stays in that
CPU's mailbox until the thread blocks or its quantum ends, and no idle CPU can
take it meanwhile. That is `main`'s behaviour for these wakes, kept by
PR #874 where it kicks every other busy target. A thread in the real-time band
woken without a boost is an ordinary wake too.

Kicking the poster's own CPU, as PR #874's third round did, ran a pass inside
the poster's call that placed every task the call woke onto that CPU. On the
T14 a 64-waiter `wake_storm_cost` storm then cost its waker 132,867 to 153,899
cycles in the three boots that carried that kick, against 52,910 to 74,328 in
50 boots of trees without it, and one of the three crossed the row's linear
bound (PR #874's fourth round). An 8-vCPU guest showed the pass's time going to the
process's fair-share lock, which the storm's other waiters were taking as
they re-parked.

Owned by stage S3 of the scheduler design (#872), which decides where a
pair's wakes go: a wake whose waker parks next is held and left here
(`issues/a-wake-held-for-its-posters-park-is-unpriced-on-metal.md`).

**Exit**: a wake posted on its own CPU by a poster that keeps running reaches
a CPU within the bound `wake_onto_busy` holds for other CPUs' wakes, on the
T14 and the AMD bench, and `wake_storm_cost` stays inside its linear bound on
both.
