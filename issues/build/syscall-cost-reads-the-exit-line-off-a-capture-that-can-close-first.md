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

**What a fix owes.** The judge waits for the line it judges by: drain the
serial until `syscalls: pid=<the spawned pid>` appears, bounded by the harness's
own wait kinds, and refuse by name if it never does — "the kernel never
accounted pid N" is a different failure from "N accounted zero". The pid is on
the spawn line the capture already carries. A fixed sleep is not the fix
(`tests/CLAUDE.md`).

**Redlist:** `src/redlist.rs` carries the sighting as `Finding::Seen`; one job
is not a rate.
