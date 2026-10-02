---
status: open
kind: defect
opened: 2026-10-02
---

# cpu0 holds interrupts and preemption off for 6.5 ms on the T14

Read by the `mask-windows` kernel (`kernel/src/windows.rs`) on LENOVO
20W0003AMZ, BIOS N34ET71W (1.71), in the eight `mask_windows` boots of #649
at `72f16e39a`: four as that head stands and four with stage 6 step 1 (#634)
reverted on it. Each boot printed one report, at `test_rs_ring_park_herd`'s
exit, so a line is its CPU's longest window from the moment it joined the
scheduler to that exit.

cpu0's line, in the seven boots where no longer window covers it:

| boot | `irqs_off_ns` | `preempt_off_ns` |
|---|---|---|
| 1, reverted | 6551791 | 6551707 |
| 2 | 6540754 | 6540671 |
| 3, reverted | 6549549 | 6549484 |
| 5, reverted | 6540326 | 6540262 |
| 6 | 6589053 | 6588992 |
| 7, reverted | 6590095 | 6590031 |
| 8 | 6587342 | 6587281 |

- The two kinds differ by 61 to 84 ns in every row, so one section holds
  both.
- It is cpu0's alone. In the six of those boots where no CPU carries a
  longer one, the other seven CPUs' interrupts-off lines read 251,452 to
  4,720,498 ns.
- It is there with step 1 and without it.
- In all eight logs cpu0 says `sched: cpu=0 ready=0 … current=None trips=1`
  at 1.155 or 1.156 s, as the CPUs join the scheduler, and nothing more
  before init's first line at 1.161 or 1.162 s.

The fourth boot's cpu0 reads 9644415 and 9643939, the event
`issues/kernel/some-t14-boots-carry-an-interrupts-off-window-of-9-ms-or-more.md`
records.

Nothing names what opens it. A report carries a span and no address, and one
report a boot cannot say whether the window recurs. What will: the record
keeping, beside each CPU's longest span, the address its opening hook was
called from, printed in the report and resolved through the kernel's symbols.

It opens before the boot's first job ends. The three boots of #649 at
`8b73eba69` (comment 5959415453, readbacks `649-r5/1-head`,
`649-r5/2-report-halved`, whose kernel prints half of every span, and
`649-r5/3-idle-halt-counted`) print a report at each of four exits. cpu0's
`irqs_off_ns` in the first, at `test_rs_idle_span`'s exit 1.746 s into the
boot (`windowscase/kernel.log:348` in each), reads 6533222, 2 × 3273547 and
6508165. In the three reports after it cpu0 reads at most 943904, 2 × 472938
and 345229.

**Exit**: the section that opens the window is named by that reading on the
T14, and cpu0's longest window there no longer includes it, or this file is
replaced by the bound the section is held to and the derivation of it.
