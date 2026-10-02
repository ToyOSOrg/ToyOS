---
status: open
kind: defect
opened: 2026-10-02
---

# init's claim of a PCI function the T14 lacks holds interrupts off for 3.8 ms

On LENOVO 20W0003AMZ, BIOS N34ET71W (1.71), one boot of `main` at `c59e09ed6`
with a throwaway instrument that names a window's opener and samples its CPU
every 2 ms (#681, comment 5962618149) read init's `SYS_DEVICE_CLAIM` for
blockd's `pci:1b36:0010`, which this machine does not have, holding cpu0's
interrupts off for 3,817,265 ns at 1.162 s, from the syscall's entry to its
return, with `MSR_SMI_COUNT` unmoved. Its one sample, 1.7 µs before it closed,
is in `PciDevice::is_id` under `pcidev::claim`. By reading, the claim reads the
vendor ID of each of the 24 functions the kernel enumerated
(`kernel/src/pcidev/mod.rs`); nothing has said what it spends 3.8 ms on.

Before the first job's exit cpu0 also carries init's partition claims, 6.0
and 6.6 ms in that boot (`issues/hardware/xhci-waits-are-spins.md`), and the
`mask-windows` kernel (`kernel/src/windows.rs`) prints each CPU's longest
window and no opener, so its cpu0 line there reads this claim only once those
have left the kernel.

**Owner**: `issues/kernel/the-kernel-is-small-interrupts-post-and-threads-wait.md`,
whose stage 5 exits on no interrupts-off window longer than a register access,
and whose step 10 takes the partition claims off cpu0.

**Exit**: after step 10, the T14's `mask_windows` row reads cpu0's
`irqs_off_ns` in its first report, at `test_rs_idle_span`'s exit, under
3,817,265 ns; or this file is replaced by the bound the claim is held to and
the derivation of it.
