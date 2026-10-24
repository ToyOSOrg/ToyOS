---
status: open
kind: defect
opened: 2026-09-04
---

# The exit record's `syscalls=` is one thread's counts under a process's name

Each thread's `TaskHandle` holds its `SyscallProfile`
(`kernel/src/sched/payload.rs`), and `teardown_resources`
(`kernel/src/process.rs`) reads only the main thread's, which `teardown` hands
it, and sends it out as the process's: in its exit record (`exit: <name> pid=N code=N cpu=Nms peak=… allocs=…
frees=… syscalls=N syscall_wall=Nms <number>=<count> …`) and in the
`ProcessStats` its exit publishes. Every other thread's calls are silently
dropped.

**Reproduced** on the dev host, 2026-09-04, when the counts were a `syscalls:
pid=N` record of their own and were those of the thread that ended the process,
from two captures of the same guest binary in one session. `exit_wait_storm`'s
parent spawns 24 children, waits for all of them and joins 24 threads; when its
main thread exits last the line is

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

**Why it matters beyond the label.** The profile is the only per-syscall
record the machine emits, and no judge reads it: nothing would notice a process
whose calls were made off its main thread.

**Exit condition.** The counters are summed across the process's threads at
teardown, or the record names the thread it is about; the doc comment matches
whichever is chosen. A gate is a guest that makes its calls on one thread and
exits from another, asserting the profile carries them.
