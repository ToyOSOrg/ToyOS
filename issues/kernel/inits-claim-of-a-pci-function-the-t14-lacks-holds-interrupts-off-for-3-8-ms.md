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

**Exit**: a T14 boot reads that claim's window, from the syscall's entry to
its return, under 223 µs, with `MSR_SMI_COUNT` unmoved across it. The bound
is an estimate: the claim's 24 vendor-ID reads at 9.3 µs each, the most a
read averaged over the last 7,871 or more of the same boot's enumeration, all
of absent functions, between `PCI 0a:00.0` at 0.072 s and `Enumeration
complete` at 0.144 s: at most 73 ms on stamps of whole milliseconds (#681,
comment 5966492598). Not every read was that fast: the 31 to 38 absent functions
behind `00:1c.4`, between `PCI 09:00.0` at 0.068 s and `PCI 0a:00.0`, took 3
to 5 ms, about 80 to 160 µs a read, and 3,817,265 ns is 159 µs for each of
the claim's 24. A claim whose reads cost that clears this bound only by making
fewer.
