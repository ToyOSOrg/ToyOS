---
status: open
kind: defect
opened: 2026-09-03
---

# A shared boot stopped answering, and no capture has ever named the waiter that parked

**What was seen.** `cargo test` (the full fast tier), dev host, 12-wide, TCG,
2026-08-29, on the branch that carried `4c7a8f7e` (the seek-past-EOF fix and its
`lseek_past_eof` test) over `48437ca4`: the shared boot stopped answering in 4 of
5 runs, `port_poll_churn` being the name the harness was waiting on. One line in
those captures is evidence rather than idle noise —
`[kernel 25.887 cpu1 tid=2] exit: test_rs_port_poll_churn tid=2 code=0 cpu=1134ms`,
a churn thread leaving cleanly, after which that guest said nothing for 179 s of
its 180 s guard.

**The re-measurement, 2026-09-03.** `main` at `66c26437` with `af64feab`
reverted, so the same workload is in the tree, and with the whole serial log
kept rather than the harness's last sixty lines (`cargo test -- --nocapture`
echoes every line of every boot, prefixed with its boot number). **Five full
fast tiers, no stop:** `303 passed, 303 total` in 369.6 s, 459.5 s, 394.3 s and
358.7 s, and `302 passed, 1 failed ... 303 total (456.7s)` in the fifth, whose
one red is `sched_check_build` refusing to read a 90th percentile off 85
samples on a loaded host — not a stop, and `ALONE ... GREEN` in the same
session. Against 4 of 5 that is 0.2^5 = 0.00032. It refutes the recorded rate
and says nothing about a rate at or below about one in five.

**`parked=N current=None` is not a stop.** Every one of those five runs carried
a healthy guest printing it for minutes: `sysret_ss_reload` boots, reaches
`===READY===`, and is asked nothing more while the host-side `drain_until`
spends its whole liveness budget (334 s, 452 s, 388 s, 352 s, 433 s in the five
runs). Its guest prints

```
[serial 18] [kernel 230.481 cpu0] sched: cpu=0 ready=0 dying=0 parked=3 current=None trips=4539
[serial 18] [kernel 230.481 cpu1] sched: cpu=1 ready=0 dying=0 parked=5 current=None trips=37524
```

and then passes. Those are the two parked counts and the reporter cadence the
2026-08-29 sighting read as proof of a wedged machine. A console read parks on a
10 ms re-poll (`CONSOLE_REPOLL`, `kernel/src/arch/syscall/io.rs`), so an idle
shared boot waiting for its next command is parked at almost every sample.

**Site: unknown.** The only candidate the surviving capture line supports is a
`SYS_THREAD_JOIN` whose wake was lost: it parks on the target thread's own watch
at `Deadline::never()` (`kernel/src/arch/syscall/proc.rs:166`), and nothing else
would leave a whole boot idle straight after a sibling thread's clean exit.
`issues/kernel/thread-exits-completion-post-is-the-second-one.md` owns that
path's two posts.

**Exit condition.** A capture taken from a boot that has actually stopped, which
names the first waiter and the subject it waits on — the blocked-task dump, or a
guest whose own last line is not the periodic reporter.
