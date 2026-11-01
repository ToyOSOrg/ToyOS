---
status: open
kind: defect
opened: 2026-10-12
---

# No row holds a wake beside seven spinners to 100 µs

A wake to a busy CPU used to wait for that CPU's next pass, and no idle CPU
could take it. PR #874 (`347e3313d`) changed that. An ordinary wake's doorbell
now kicks a busy target on its edge (`Doorbell::ring`,
`kernel/pure/sched/mailbox.rs`), and at the drain `place`
(`kernel/pure/sched/cpu.rs`) forwards a fair task that would queue behind a
running fair task to an idle CPU. A task that stays and is due first cuts the
running request at its minimum run, `MIN_SLICE_NS` (250 µs) after its dispatch
(`kernel/pure/sched/queue.rs`), and one queued task counts as surplus to a
steal (`PUSH_THRESHOLD` = 1).

**What #874 measured.** The metal row `wake_onto_busy` (`tests/toyos.rs`)
times a wake onto a CPU that one spinner holds, beside CPUs with nothing to
run, and reds at a p99 of 1 ms (`WAKE_ONTO_BUSY_P99_US`). At #874's head it
read p99 5 µs on the T14 and 25 µs on the AMD bench, with 200 of 200 wakes run
off the busy CPU on each. With the kick reverted it read p99 10,001 µs on both
(https://github.com/ToyOSOrg/ToyOS/pull/874).

**The row's staging can be undone by the kernel.** On the T14, at PR #1071's
`4f19eae7e`, whose branch changes no kernel, abi, std or scheduler code,
`wake_onto_busy` refused `NotStaged` (exit -5) with "the waker left CPU 3 for
6": the kernel moved the spinning waker after staging, so that run measured
nothing. The 36 T14 readbacks of it from 2026-10-13 to 2026-10-27 each measured
a p99 (4 to 30 µs). Why a CPU-bound thread was moved while other CPUs sat idle
is not established; a row that reds on it would be reading placement, not the
wake.

**What is still owed.** This file's own arm is a wake beside seven CPU-bound
spinners on eight CPUs, so one CPU is left idle, held to a post-to-dispatch
p99 under 100 µs. No row runs that shape or holds that bound: `wake_onto_busy`
has one spinner and a bound ten times wider. With every CPU busy, the wake is
`issues/a-woken-thread-waits-out-a-sibling-spinners-quantum-when-every-cpu-is-busy.md`'s;
a wake posted on the poster's own CPU is
`issues/an-ordinary-wake-posted-on-its-own-cpu-waits-for-that-cpus-next-pass.md`'s.

**Measured before #874** (`wt/toyos-scout-sys` at `fbb958b17`, `main`'s
`177e72a9e` plus the scout;
https://github.com/ToyOSOrg/ToyOS/pull/866#issuecomment-6119438569). A
ping-pong pair beside seven CPU-bound spinners on eight CPUs, timed from post
to dispatch on the kernel clock; quantiles are bucket upper bounds, accurate to
12.5 %:

| bench | target asleep, p99 | target busy, p50 / p99 / max |
|---|---|---|
| T14 | ≤2.30 µs | ≤10.49 ms / ≤10.49 ms / 10.00 ms |
| AMD bench | ≤2.56 µs | ≤10.49 ms / ≤10.49 ms / 10.00 ms |

This was the Overload-on-Wakeup bug of Lozi et al., "The Linux Scheduler: a
Decade of Wasted Cores" (EuroSys 2016, §3.3): a thread that was asleep wakes on
an overloaded core while other cores are idle. Its fix wakes the thread on the
core it last ran on if that core is idle, and otherwise on the core idle
longest, where one is. The paper applies it only where the power policy keeps
cores out of low-power states, since waking a core from one costs power. This
arm does not take that condition: a wake goes to an idle core in whatever state
it waits, so once `issues/idle-cpus-enter-the-c-states-cst-names.md` lets cores
sleep deeper, the row's p99 also holds the woken core's exit from its state,
and the row names the states its idle CPUs entered. The 100 µs bound is the
exit's test that the stage works, not the judge of its design.

**Exit**: a metal row on the T14 and the AMD bench that wakes a task onto a
CPU running a CPU-bound thread, with seven such threads on eight CPUs, and
holds the post-to-dispatch p99 under 100 µs.
