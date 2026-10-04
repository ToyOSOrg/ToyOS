---
status: open
kind: finding
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

Not yet attributed: the row's own reader runs between the two reads, prints
its `idle0` lines and parks, and which CPUs it and the log's path ran on that
second is not recorded. **Exit**: a reading that names what ran on cpu0 and
cpu7 across that second, and either it is the row's own work, folded to the
row's doc, or it is promoted to a defect with its cause.
