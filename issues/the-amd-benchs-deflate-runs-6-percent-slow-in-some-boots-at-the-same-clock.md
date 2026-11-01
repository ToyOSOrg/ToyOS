---
status: open
kind: finding
opened: 2026-10-28
---

# The AMD bench's deflate runs 6 % slow in some boots at the same clock

osbench's `crunch.single.deflate` on the AMD bench (Ryzen 7 4800U) has more
than one value across twelve boots (`ToyOSOrg/evidence` branch `pr-1073`,
`r3/` to `r8/`):

- In three boots its p50 was 66.6 to 66.8 ms. Two ran the thread on CPU 4
  and one on CPU 10.
- In one, B6's, it was 65.7 ms, on CPU 0.
- In the eight others it was 62.9 to 63.7 ms, on CPUs 0, 2, 6, 10 and 12.

The thread's APERF/MPERF ratio, the one figure of the pair AMD defines
(APM Vol. 2 §17.3), differed by under 1 % across the boots. The slow boots
include one with every idle CPU in C1 and the rest with the deep states.
In the slow A\* boot and the slow B4 boot, every other CPU's MPERF advanced
under a thousandth of the deflate thread's own across the row (0 against
999, each in thousandths of the same window), and MPERF counts only in C0
(§17.3).
So it is not the idle states, the clock, the placement, or a running
neighbour.

`sha256`, `matmul` and `parse` show no such split. Every AMD single-thread
timing row may carry this spread. A comparison between two AMD boots,
ToyOS's or Linux's, can therefore read 6 % that neither system caused.

**Exit**: one boot running deflate several times, at intervals, on one
pinned CPU, which says whether the mode is per boot or per run, and with the
package's power and temperature beside it once a counter carries them.
