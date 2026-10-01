---
status: open
kind: tooling
opened: 2026-10-01
---

# An idle guest and a hung one print the same `sched:` line

`blockd_serves_partitions`' guest (`650-libcllvm-whole.log`) logged
`sched: cpu=1 ready=0 … parked=4 current=None` every ten seconds for an hour
with its test's end never reported
(`issues/kernel/a-job-that-exited-was-never-reported-ended.md`). The
harness's ceiling read that as a guest still talking. Only the backstop could
end it, at the test's own 600 s once a stopped guest's clock runs at the
wall's rate.

The line cannot do better. `scheduler::log_health` counts parked threads and
says nothing of what each waits on. A guest waiting out a timer, such as
`lan_no_lease` riding netd's 20 s lease bound or a job asleep under its
deadline, prints the same line as one whose every thread waits on a wake that
will never come. A wait that ended a run on `ready=0` would red the first kind.

**Exit**: the kernel's idle line also counts the parks that carry a deadline,
and a wait on a guest ends, by name, once every CPU has reported `ready=0`
with no such park for the quiet span. The case it ends early is this file's
sighting.
