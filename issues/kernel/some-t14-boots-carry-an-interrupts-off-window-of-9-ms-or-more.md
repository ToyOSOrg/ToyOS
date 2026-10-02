---
status: open
kind: defect
opened: 2026-10-02
---

# Some T14 boots carry an interrupts-off window of 9 ms or more

Read by the `mask-windows` kernel (`kernel/src/windows.rs`) on LENOVO
20W0003AMZ, BIOS N34ET71W (1.71), in the eight `mask_windows` boots of #649
at `72f16e39a`: four as that head stands and four with stage 6 step 1 (#634)
reverted on it, one job each, `test_rs_ring_park_herd`. The longest window
that recurs in every boot is cpu0's 6.5 ms
(`issues/kernel/cpu0-holds-interrupts-and-preemption-off-for-6-5-ms-on-the-t14.md`).
Three readings are longer, each in some boots and not in others, on both
arms:

- **Every CPU at once, the fourth boot** (step 1 in). The herd's report:

  | CPU | `irqs_off_ns` | `preempt_off_ns` |
  |---|---|---|
  | cpu0 | 9644415 | 9643939 |
  | cpu1 | 9603617 | 9588515 |
  | cpu2 | 9627971 | 9625674 |
  | cpu3 | 9641999 | 9620791 |
  | cpu4 | 9651846 | 9635137 |
  | cpu5 | 9623545 | 9612295 |
  | cpu6 | 9624969 | 9621345 |
  | cpu7 | 9614836 | 9614716 |

  The same report's shootdown line reads
  `tlb: shootdowns=280 wait=125911us max=9575us`, where the other seven
  boots read at most `wait=49119us` and at most `max=1421us`, and cpu0's
  census reads `nmi=2`, where the other seven read `nmi=1`.
- **One CPU, the seventh boot** (step 1 reverted). cpu7 reads 9260193 and
  9260124. cpu0 reads its 6590095, the other six 920,455 to 1,404,001, and
  the shootdown line `max=1386us`.
- **One CPU, after the herd's report, in four boots of the eight.** The stop
  prints a report too, of which the black box keeps the lines of cpu2 to
  cpu7. In it one CPU reads

  | boot | CPU | `irqs_off_ns` | `preempt_off_ns` |
  |---|---|---|---|
  | 1, reverted | cpu7 | 10677997 | 10677829 |
  | 2 | cpu7 | 11267384 | 11267320 |
  | 5, reverted | cpu2 | 10214820 | 10214656 |
  | 6 | cpu7 | 10874427 | 10874364 |

  and in the other four cpu7 reads 1,424,134 to 1,532,702 ns and no kept
  line more. What that report spans is the herd's teardown, which its own
  report precedes, the spawn of `reboot` on cpu7 and the stop.

By reading, not measured: every CPU waiting out one CPU's masked window
inside a shootdown would read as the fourth boot does.

Nothing names what opens any of them. A report carries a span and no address,
and no time: eight equal spans are not shown to be one event. What will: the
record keeping, beside each CPU's longest span, the address its opening hook
was called from and the counter it closed at, printed in the report.

The third reading falls on the `reboot` and stop side of the herd's
teardown. The three boots of #649 at `8b73eba69` (comment 5959415453,
readbacks `649-r5/1-head`, `649-r5/2-report-halved`, whose kernel prints half
of every span, and `649-r5/3-idle-halt-counted`) took a report at `echo`'s
exit, after the herd's and before `reboot` was spawned. Its longest
`irqs_off_ns` is cpu7's in each: 1499808, 2 × 748872 and 1479558
(`windowscase/kernel.log:456`). The stop's report after it, in the six lines
the black box keeps, reads cpu7 11522834 and cpu4 11645411 in the first boot
(`649-r5/1-head/windowscase/loader.log:57`, `:63`), and at most cpu7's
2 × 3799327 in the second and cpu7's 1305656 in the third.

The three boots at `0aa8d4c88` (comment 5960575031, readbacks
`649-r6/1-head`, `649-r6/2-report-halved` and `649-r6/3-idle-halt-counted`,
the same two mutated kernels) print a report at three exits, `idle_span`'s,
`pwd`'s and the herd's, and at the stop:

- **The first reading recurred, inside the herd's own report**, which opens
  at `pwd`'s exit. In `2-report-halved` every CPU reads 2 × 5014552 to
  2 × 5075290 (`windowscase/kernel.log:416` to `:430`), beside
  `tlb: shootdowns=282 wait=131633us max=10038us` (`:431`): the shootdown's
  `max` is beside it again, 9575us then and 10038us now, and cpu0's second
  NMI is not: `nmi=1` (`:415`). That is two boots of #649's fourteen.
- **One CPU, before the first job's exit.** cpu7's line of the first report,
  `idle_span`'s (`:362`), reads 11492028 in `1-head`, 2 × 1308074 in
  `2-report-halved` and 8456658 in `3-idle-halt-counted`, beside
  `max=1655us`, `max=1451us` and `max=1353us` (`:363`). The same line read
  2327848, 2 × 1831727 and 3630933 at `8b73eba69`.
- **Nothing after the herd's report.** The stop's report, in the six lines
  the black box keeps, reads at most cpu7's 1396887, 2 × 758960 and 1470910
  (`windowscase/loader.log:57`).

**Exit**: each of the three, and the one-CPU reading before the first job's
exit, is named by that reading on the T14, and is removed, or this file is
replaced by the bound it is held to and the derivation of it.
