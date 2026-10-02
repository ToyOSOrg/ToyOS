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
`issues/kernel/a-log-write-to-the-t14s-stick-masks-interrupts-for-its-whole-usb-transfer-on-one-cpu-or-on-all-eight.md`
records.

It is init in `SYS_DEVICE_CLAIM`. One boot of `main` at `c59e09ed6` with a
throwaway instrument that names a window's opener and samples its CPU every
2 ms (its lines quoted on the pull request that landed this text) read cpu0's
line at 6623907 and 6623757 and named that window: pid 0, opened at the
syscall's entry and closed at its return, from 1.179 s, both samples in
`XhciController::wait_transfer` under `storage_read`. Two more of init's
claims precede it on cpu0: one at 1.171 s of 6,002,063 ns, both samples in
`wait_transfer` under `storage_read`, and one at 1.162 s of 3,817,265 ns,
whose one sample is in `PciDevice::is_id` under `pcidev::claim`. By reading, a
partition claim reads its disk's table when asked (`gpt::claimable`,
`kernel/src/gpt.rs`) through the same masked USB wait as that event's. Nothing
has said what the PCI claim spends 3.8 ms on.

It opens before the boot's first job ends. The three boots of #649 at
`8b73eba69` (comment 5959415453, readbacks `649-r5/1-head`,
`649-r5/2-report-halved`, whose kernel prints half of every span, and
`649-r5/3-idle-halt-counted`) print a report at each of four exits. cpu0's
`irqs_off_ns` in the first, at `test_rs_idle_span`'s exit 1.746 s into the
boot (`windowscase/kernel.log:348` in each), reads 6533222, 2 × 3273547 and
6508165. In the three reports after it cpu0 reads at most 943904, 2 × 472938
and 345229.

**Exit**: on the T14, cpu0's longest window before the first job's exit no
longer includes init's claims, or this file is replaced by the bound they are
held to and the derivation of it.
