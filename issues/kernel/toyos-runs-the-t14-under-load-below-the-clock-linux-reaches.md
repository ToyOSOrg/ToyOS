---
status: open
kind: defect
opened: 2026-10-04
---

# ToyOS runs the T14 under load below the clock Linux reaches

The `counters` metal row, at `ce1786ff0` (pull request #705), spun one thread
per CPU for its `spin` phase and read every CPU's busy clock as 2318 MHz
(TSC 2419 MHz times APERF over MPERF, MPERF at 0.986 to 0.990 of the stamp,
so every CPU was in C0 the whole span). Linux's turbostat on the same machine
under one `yes` per CPU (`tests/t14-linux/turbostat-loaded.txt`) reads
3800 MHz for its first 20 s and 3075 to 3094 MHz once the package settles near
20 W. ToyOS runs a loaded T14 at 61 to 75% of Linux's busy clock, below the
TSC's own rate.

The CPU enumerates HWP (`hwp`, `hwp_epp` in
`toyos-cpuvuln/fixtures/t14/cpuinfo.txt`), and the kernel writes neither
`IA32_PM_ENABLE` nor `IA32_PERF_CTL`: the clock is whatever the firmware left.
`issues/kernel/the-scheduler-and-the-clocks-never-talk.md` is the track that
would couple the scheduler to frequency; this is the floor beneath it, a
machine that never leaves the firmware's operating point.

Owner: the orchestrator, which holds the T14. **Exit**: the `counters` row
holds every CPU's busy clock across its spin to
Linux's loaded range in `tests/t14-linux/`, and the reading that held it is in
the closing pull request.
