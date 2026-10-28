---
status: open
kind: finding
opened: 2026-10-28
---

# The AMD bench's deflate runs 6 % slow on CPU 4 at the same clock

osbench's `crunch.single.deflate` on the AMD bench (Ryzen 7 4800U) has two
values across eight boots. In the two boots whose deflate thread ran on
CPU 4, its p50 was 66.6 and 66.8 ms. In the six boots that placed it on
CPUs 0, 2, 10 or 12, its p50 was 62.9 to 63.6 ms. APERF/MPERF put the
thread's clock at 4214 to 4250 MHz in every boot. The two slow boots were
one with the deep idle states and one without them, with every idle CPU in
C1 (`ToyOSOrg/evidence` branch `pr-1073`, `r3/`). Idle states do not explain
it.

`sha256`, `matmul` and `parse` show no such split, including the one boot
that ran `sha256` on CPU 4. Two boots are too few to tell whether CPU 4
itself is slower, or whether that placement shares the core or cache with
other work.

Every AMD single-thread timing row can carry this spread. A comparison
between two AMD boots, ToyOS's or Linux's, that does not hold the placement
equal can read 6 % that neither system caused.

**Exit**: in one boot, deflate pinned to each CPU in turn, with each
sibling's and each CCX's load noted, either names the cause or shows that
the spread follows the core.
