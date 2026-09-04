---
status: open
kind: defect
opened: 2026-09-04
---

# `syscalls: pid=N` is one thread's counts under a process's name

`ThreadData` holds `syscall_counts`, `syscall_total` and `syscall_total_ns`
(`kernel/src/process.rs:565-575`), and `teardown_resources` reads them from the
one `thread_data_arc` its caller handed it and prints them as the process's
(`kernel/src/process.rs:932-942`). `release_process` hands it the *current*
thread's (`kernel/src/process.rs:1115`, `:1130`), so the line reports whichever
thread ended the process and silently drops every other thread's calls. Its own
doc comment says "for the main thread" (`kernel/src/process.rs:911`), which is
false on that path.

**Reproduced** on the dev host, 2026-09-04, from two captures of the same guest
binary in one session. `exit_wait_storm`'s parent spawns 24 children, waits for
all of them and joins 24 threads; when its main thread exits last the line is

```
syscalls: pid=7 total=204 syscall_wall=516ms 0=1 6=1 8=2 10=24 25=24 40=25 41=24 50=24 63=26 72=1 73=2 91=1 99=1 102=24 108=24
```

and when its watchdog thread calls `exit` first, the same workload reports

```
syscalls: pid=7 total=14 syscall_wall=3093ms 0=12 49=1 72=1
```

— fourteen calls for a process that had made two hundred, with no spawn, no
wait and no join in the profile, and `syscall_wall` reading the watchdog's
3 s sleep as the process's syscall time.

**Why it matters beyond the label.** The line is the only per-syscall record
the machine emits, and `tests/toyos.rs`'s `check_syscall_cost` and
`check_exit_wait_storm` both judge a guest against it. Both happen to read a
single-threaded claim made by the main thread, so both are sound today — and
neither would notice the day the thread that ends the process is not the one
that made the calls.

**Exit condition.** The counters are summed across the process's threads at
teardown, or the line names the thread it is about; the doc comment matches
whichever is chosen. A gate is a guest that makes its calls on one thread and
exits from another, asserting the profile carries them.
