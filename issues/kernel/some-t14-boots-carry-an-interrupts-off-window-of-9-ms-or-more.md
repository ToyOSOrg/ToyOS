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
was called from and the counter it closed at, printed in the report. The row
now takes a report after the herd's exit and before `reboot` is spawned,
which says on which side of that the third reading falls.

**Exit**: each of the three is named by that reading on the T14, and is
removed, or this file is replaced by the bound it is held to and the
derivation of it.
