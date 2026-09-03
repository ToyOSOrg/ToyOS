---
status: open
kind: defect
opened: 2026-09-03
---

# `syscall_cost` reads the kernel's exit accounting off a capture that can close before the kernel prints it

`check_syscall_cost` (`tests/toyos.rs`) judges the workload against a counter
the guest program cannot reach: the `syscalls: pid=N … 8=<count>` line the
kernel prints from `teardown_resources` (`kernel/src/process.rs`) when the
process is torn down. The harness reads the program's exit code and then reads
whatever the serial capture holds. Nothing waits for that line.

Measured on `ci.yml` run 33731006452, `guest (2)`, pull request #380, whose
diff touches neither syscall accounting nor the log:

```
FAIL rs::syscall_cost: the run claims 180000 SYS_CLOCK transitions and the kernel counted 0
syscall_cost: 706 cycles/syscall over 9x20000
syscall_cost: tsc 2298 MHz
[kernel 22.881 cpu0] spawn: /bin/test_rs_syscall_cost pid=212 tid=0 … (layout=0ms … total=0ms)
FAIL syscall_cost: exit code Some(0)
…
ALONE syscall_cost: red again, the same failure both times — the defect is real. exit code Some(0)
```

Both attempts and the alone re-run carried the spawn line and **no
`syscalls: pid=` line at all** for the process, so `counted` fell to its
`unwrap_or(0)`. The alone re-run's spawn line reads `layout=2855ms` — a host
slow enough that the teardown's log line landed after the harness had taken its
capture. On the dev host the same tree passes (`[syscall] 1152 cycles per
SYS_CLOCK over 217409 of them`), which is the shape of a race and not of a
counter that did not move.

## The mechanism, read off the harness

`run_test_paced` (`tests/common/qemu.rs`) hands the `TestResult` back the moment
the guest runner prints `===TEST_END <name> exit=<code>===`, and the runner
prints that when it reaps the child. The accounting line comes from
`teardown_resources` (`kernel/src/process.rs`), which
`issues/kernel/deferred-release-outlives-its-syscall.md` records as able to run
*after* the syscall that caused it returned. Both orders are legal, the host
decides which, and nothing in between waited.

The judge made that indistinguishable from a real zero: it read **every**
`syscalls: pid=` line in the capture, took the `.max()`, and put `unwrap_or(0)`
behind it — so a line that had not arrived and a process that made no calls were
one number.

## What it now does

The wait is in the test's driver, where the instance still is:
`settle_syscall_cost` returns at once when the line is already there — every run
measured on the dev host — and otherwise drains until it appears, bounded by
`drain_until`'s host-scaled liveness ceiling rather than a sleep. Its expiry is
not the verdict. `check_syscall_cost` keys on the pid off the spawn line and
refuses **by name** when the line never came, which is a different sentence from
a count that fell short.

**Both arms, with the kernel's `syscalls:` line muted so the absence is staged.**
The race itself could not be staged: the line was already in the capture in 5 of
5 local runs, so the drain never waited.

    old judge: FAIL rs::syscall_cost: the run claims 180000 SYS_CLOCK
      transitions and the kernel counted 0
    new judge: FAIL rs::syscall_cost: the kernel never accounted the process —
      no `syscalls: pid=7 ` line reached the capture, so nothing here says
      whether it made the calls it claims

The first is what CI reported, word for word, which is what says the staged
absence is the same observable the race produces.

**Redlist:** `src/redlist.rs` carries it as `Finding::fires(3, 5)` over every
hosted run of the name. It stands until a hosted run is green with the wait in
it, and this file closes with that row.
