---
status: open
kind: finding
opened: 2026-10-28
---

# The AMD bench's deflate runs 6 % slow in some boots at the same clock

osbench's `crunch.single.deflate` on the AMD bench (Ryzen 7 4800U) has two
values across nine boots (`ToyOSOrg/evidence` branch `pr-1073`, `r3/` and
`r4/`):

- In three boots its p50 was 66.6 to 66.8 ms. Two ran the thread on CPU 4
  and one on CPU 10.
- In the six others it was 62.9 to 63.6 ms, on CPUs 0, 2, 10 and 12.

APERF/MPERF put the thread's clock at 4214 to 4250 MHz in every boot. The
slow boots include one with every idle CPU in C1 and two with the deep
states. In the slow A\* boot and the slow B4 boot, every other CPU, the
sibling included, read `busy_permille=0` across the row. So it is not the
idle states, the clock, the placement, or a running neighbour.

`sha256`, `matmul` and `parse` show no such split. Every AMD single-thread
timing row may carry this spread. A comparison between two AMD boots,
ToyOS's or Linux's, can therefore read 6 % that neither system caused.

**Exit**: one boot running deflate several times, at intervals, on one
pinned CPU, which says whether the mode is per boot or per run, and with the
package's power and temperature beside it once a counter carries them.
