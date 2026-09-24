---
status: open
kind: defect
opened: 2026-09-22
---

# A held disk waits for a pass no CPU takes when every CPU is in a call on it

A disk whose device left under this driver's reset is held for the device to
come back, and a call on it only waits for the verdict: the returning device is
enumerated and bound by the port machine on a CPU that reaches a scheduler pass
(`kernel/src/drivers/xhci/wait/msc.rs`, `wait_for_return`). A disk call spins
with `IF` clear, so when every CPU is inside a call on the held disk, no CPU
takes that pass until the calls end on their bounds, and each answers
`BudgetExpired`.

**Measured under QEMU, `smp:2`**: `usb_transport_break`'s moved-stick boot at
`99a81a8c` (`utb-rateb2-usbtransport-9.log` in the job scratchpad): cpu0's
write and cpu1's read of disk 0 both held from 0.356 s; port 3 read connected
throughout and was first enumerated at 2.455 s, after both calls ended at
2.354 s. At that head the window also ran out first and disk 0 was lost; since
`f0695038` a held disk is not forgotten while a port reads connected and
untaken, so the disk is taken back and the retried operations complete (1 of 6
runs at `f0695038` took this shape and passed).

**What is still wrong**: the operation that waited answers `BudgetExpired`
instead of the device's answer, and a caller that gives up on one — `logd` on a
refused create (`issues/boot-media/logd-ends-the-boots-log-on-one-refused-create-and-nothing-durable-says-so.md`)
— loses what it was doing. The retry loops above the block layer spin with `IF`
clear too, so on a machine with one CPU the device binds only when the caller
leaves the kernel.

**A demand fill spins across calls.** `file_backing::read_block_retrying`
retries `BudgetExpired` for up to `block::DEADMAN` (120 s) without leaving the
kernel, since a fill cannot park. A fill on a held disk therefore spins call
after call: each is inside `CALL_AFTER_BREAK`, and their sum is bounded only by
the deadman. Derived from the code in review round 2 of #466, not staged.

**Not staged**: the `ARRIVED` mutation (the arrival rule removed) survives a
run of `usb_transport_break`, because nothing makes every CPU enter a call on
the held disk; the interleaving comes at a rate.

**Exit**: a disk call that waits for a device does not hold a CPU with `IF`
clear — the wait parks — or a held call's CPU may bind within its bound without
making the bind the caller's cost. Either is the owner's ruling to revisit.
