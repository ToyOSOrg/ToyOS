---
status: open
kind: defect
opened: 2026-10-02
---

# init's claim of a PCI function the T14 lacks holds preemption off for 3.8 ms

On LENOVO 20W0003AMZ, BIOS N34ET71W (1.71), one boot of `main` at `c59e09ed6`
with a throwaway instrument that names a window's opener and samples its CPU
every 2 ms (#681, comment 5968784527) read init's `SYS_DEVICE_CLAIM` for
blockd's `pci:1b36:0010`, which this machine does not have, holding cpu0's
interrupts off for 3,817,265 ns at 1.162 s, from the syscall's entry to its
return, with `MSR_SMI_COUNT` unmoved. Its one sample, 1.7 µs before it closed,
is in `PciDevice::is_id` under `pcidev::claim`. By reading, the claim reads the
vendor ID of each of the 24 functions the kernel enumerated
(`kernel/src/pcidev/mod.rs`); nothing has said what it spends 3.8 ms on.

A syscall's body now runs with interrupts open and preemption off
(`issues/syscall-preemption-is-incidental.md`). #716's interleaved
`mask_windows` boots (comment 5979107466; readbacks
`irqon/metal/{base,head}/mask_windows/runN` and `irqon/metal/head-full`) each
made this claim (`supervisor: diskserver: no pci:1b36:0010 on this machine`)
before the first job's report, which spans it and the partition claims below.
cpu0's line there:

| arm | `irqs_off_ns` | `preempt_off_ns` |
|---|---|---|
| base, main at `d47b383cf` | 6592629, 6565352, 6491279, 6614740, 6509061 | 6592463, 6565169, 6491091, 6614595, 6508897 |
| head, images at `f89e73128` | 3061, 10315, 14214, 15492, 3510; 15409 | 6550562, 6517919, 6632892, 6549746, 6619444; 5706681 |

On the head no CPU but the one `hold_once` held reads more than 44537 ns of
interrupts off in that report, which bounds every window inside the claim
from above; no reading separates the claim's own windows from the rest.

Before the first job's exit cpu0 also carries init's partition claims, 6.0
and 6.6 ms in that boot (`issues/xhci-waits-are-spins.md`), and the
`mask-windows` kernel (`kernel/src/windows.rs`) prints each CPU's longest
window and no opener, so its cpu0 line there reads this claim only once those
have left the kernel.

**Owner**: `issues/the-kernel-is-small-interrupts-post-and-threads-wait.md`,
whose stage 5 exits on no interrupts-off window longer than a register access,
and whose step 10 takes the partition claims off cpu0.

**Exit**: a T14 boot reads the longest interrupts-off window inside that
claim no longer than one register access, with `MSR_SMI_COUNT` unmoved across
that window. A figure for that access comes from the claim's own reads timed
on the T14, and nobody has timed them yet.
