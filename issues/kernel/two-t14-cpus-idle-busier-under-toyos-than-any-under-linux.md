---
status: open
kind: defect
opened: 2026-10-04
---

# Two T14 CPUs idle busier under ToyOS than any under Linux

The `counters` metal row, at `ce1786ff0` (pull request #705), read each CPU's
MPERF over its stamp across the idle second between its `idle0` and `idle1`
reads: cpu0 1.37% and cpu7 1.45% busy, cpu3 0.11%, the other five 0.00 or
0.01%. Linux's turbostat on the same machine, idle
(`tests/t14-linux/turbostat-idle.txt`), reads 0.11 to 0.36% machine-wide per
10 s and no CPU above 0.69%. So six CPUs idle deeper than Linux's and two
about four times busier than Linux's machine-wide busiest interval.

A second boot, at `a059144e2`, read the same two CPUs again: cpu0 1.35% and
cpu7 1.42%, cpu4 0.37%, cpu3 0.11%, the other four 0.00 to 0.03%. The two
busy CPUs reproduce across boots.

**cpu0, named in another window.** A diagnostic boot of pull request #717's
`trace_read` (commit `53f75eacb`, its readback quoted on #717) recorded why
each idle pass declined to halt: across about 5.3 ms cpu0 wrote 4,613
declines whose only reason was an undrained `irq_ring` record and 4,618
`TimerArm`s, and every other CPU 87 records or fewer. An xHCI record is the
only one a pass leaves: `xhci::poll_if_pending` declines a taken `XHCI` and
leaves it, and a USB-stick transfer holds `XHCI` on another CPU for as long as
the device takes, so cpu0, where every device interrupt lands, ran pass after
pass until the transfer's CPU let go. `XHCI` is an `OwedLock` since: a
declined CPU halts and the release kicks it. That window is not the row's idle
second, and nothing has yet read cpu7's.

Not yet attributed: the row's own reader runs between the two reads, prints
its `idle0` lines and parks, and which CPUs it and the log's path ran on that
second is not recorded. Owner: the orchestrator, which holds the T14.
**Exit**: a reading that names what ran on cpu0 and
cpu7 across that second; then the cause is fixed, or, if it is the row's own
work, folded to the row's doc.
