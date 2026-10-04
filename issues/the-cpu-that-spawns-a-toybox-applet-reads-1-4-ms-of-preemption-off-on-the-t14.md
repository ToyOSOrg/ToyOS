---
status: open
kind: defect
opened: 2026-10-02
---

# The CPU that spawns a toybox applet reads 1.4 ms of preemption off on the T14

Read by the `mask-windows` kernel (`kernel/src/windows.rs`) on LENOVO
20W0003AMZ, BIOS N34ET71W (1.71), in the three `mask_windows` boots of #649
at `8b73eba69` (comment 5959415453, readbacks `649-r5/1-head`,
`649-r5/2-report-halved`, whose kernel prints half of every span, and
`649-r5/3-idle-halt-counted`, whose kernel counts the idle halt as a
preemption-off window).

test-runner spawned every job from cpu7: each `spawn:` record of a job is
cpu7's. Two reports a boot span the spawn of a toybox applet and of nothing
else, `pwd`'s and `echo`'s. cpu7's line in each
(`windowscase/kernel.log:388` and `:456`):

| boot | report | `irqs_off_ns` | `preempt_off_ns` |
|---|---|---|---|
| `1-head` | `pwd`'s | 1408843 | 1408557 |
| `1-head` | `echo`'s | 1499808 | 1499590 |
| `2-report-halved` | `pwd`'s | 2 × 737408 | 2 × 737277 |
| `2-report-halved` | `echo`'s | 2 × 748872 | 2 × 748704 |
| `3-idle-halt-counted` | `pwd`'s | 1379642 | 536504022, a halt |
| `3-idle-halt-counted` | `echo`'s | 1479558 | 1479398 |

- The two kinds differ by 160 to 336 ns in the five rows that have both, so
  one section holds both.
- No other CPU reads as much in `echo`'s report: 947018 at most.
- It is the floor under a load's reading on that CPU. A load's own report
  spans the runner's spawn of it, and in the herd's cpu7 reads 1019488,
  2 × 514392 and 1175943, the last the longest line of its report by three
  times.
- It is not all that line reads. In the three boots at `0aa8d4c88` (comment
  5960575031, readbacks `649-r6/1-head`, `649-r6/2-report-halved` and
  `649-r6/3-idle-halt-counted`, the same two mutated kernels) `pwd`'s report
  reads cpu7 at 1511452, 2 × 740441 and 1360995
  (`windowscase/kernel.log:388`), and the herd's reads it at 1980239 in
  `1-head` (`:430`): 468787 past that boot's applet spawn, which the spawn
  does not account for and nothing names.

By reading, not measured: it is `SYS_SPAWN`, which ran with interrupts
masked from entry to exit like every syscall then, and whose own record
reads `total=1ms` for each of these applets. A report carries a span and no
address, so nothing names it.

A syscall's body now runs with interrupts open and preemption off
(`issues/syscall-preemption-is-incidental.md`), and the section left
`irqs_off_ns` and stayed in `preempt_off_ns`. #716's interleaved
`mask_windows` boots (comment 5979107466; readbacks
`irqon/metal/{base,head}/mask_windows/runN` and `irqon/metal/head-full`) ran
`pwd` alone of the applets, spawned from cpu7 on every boot; cpu7's line in
its report (`windowscase/kernel.log:396`, `:397` in `head-full`):

| arm | `irqs_off_ns` | `preempt_off_ns` |
|---|---|---|
| base, main at `d47b383cf` | 1429697, 1334122, 1460438, 1385896, 1373747 | 1429402, 1333862, 1460269, 1385648, 1373471 |
| head, images at `f89e73128` | 1146, 1178, 1136, 1781, 1058; 1155 | 1317184, 1508859, 1463782, 1494243, 1511276; 1481928 |

**Exit**: the section is named on the T14 by the address its opening hook was
called from, and the spawning CPU's longest window no longer includes it, or
this file is replaced by the bound it is held to and the derivation of it.
