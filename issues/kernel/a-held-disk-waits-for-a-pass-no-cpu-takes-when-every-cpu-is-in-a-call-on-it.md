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

**A CPU spinning on a lock the call's caller holds counts too.**
`usb_transport_break --nightly` at `e889d03e` (#554), the `AnotherStick` boot
(`554r6-usb_transport_break.log` in the job scratchpad), red. cpu0 spent 0.383
to 4.385 s in logd's create of the boot's log file, which holds `vfs::lock()`
(`object::ops::open`): two writes of block 9351 on held disk 0, each ending
`still held` on its bound, at 2.384 and 4.385, with no pass between them.
cpu1 logged nothing from 0.311 to 4.390 s. Two threads resumed within a
millisecond of that create's end: `test-runner`'s spawn of `reboot`, whose own
`total=7ms` puts its start at about 4.383, and init's `started test-runner`
line, stamped 4.386 for a spawn made at 0.363. An idle cpu1 kicked by
`wait_for_return` would have torn port 1 down within its 100 ms debounce, and
no `port 1 disconnected` line exists, so cpu1 took no pass. That it spun on the
VFS lock is inferred, not measured. Port 3 read connected and untaken
throughout, so the arrival rule kept disk 0 held and no `did not come back`
line came either. The other stick was never enumerated, and the test's
`is not disk 0 come back` line never came. From 4.424 s cpu0 spun in the
reboot's sync and cpu1 in a shootdown
(`a-shutdown-on-a-held-usb-disk-left-a-cpu-deaf-to-a-tlb-shootdown.md`).

**What is still wrong**: the operation that waited answers `BudgetExpired`
instead of the device's answer, and a caller that gives up on one — `logd` on a
refused create (`issues/boot-media/logkeeper-ends-the-boots-log-on-one-refused-create-and-nothing-durable-says-so.md`)
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

Related records — `usb_transport_break`'s three other open red modes, not this
one: `issues/kernel/a-shutdown-on-a-held-usb-disk-left-a-cpu-deaf-to-a-tlb-shootdown.md`,
`issues/build/usb-transport-break-flushedstick-can-break-after-the-reboot.md`, and
`issues/boot-media/a-disk-whose-port-went-away-panics-the-boot-at-roots-hold.md`.

**Unrun since it was disabled**: PR #562 deleted this test's timing check,
that the staged break's record came less than 2 s of kernel clock after the
record before it. That change has never run: the test's first run back is
also that change's.

**`4f2bea143` holds #588's version of the test**: `git show 4f2bea143:tests/common/usb.rs`.
`usb_stick_left`'s T14 row arms `usb-transport-break`, so it stays; `usb-reset-moves`,
`usb-reset-moves-after` and `usb-reset-moves-configured` went with its QEMU arm in #660.
