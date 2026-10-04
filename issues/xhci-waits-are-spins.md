---
status: open
kind: defect
opened: 2026-08-03
---

# The xHCI driver's waits are spins, under a lock and with preemption off

Every wait in the kernel's xHCI driver spins against a wall-clock deadline
while holding `XHCI` (`kernel/src/drivers/xhci/mod.rs`), a ticket spinlock and
so preemption off for its whole life: `settles()` for the controller's halt,
reset and port reset, and `wait_command()` and `wait_transfer()` for every
command and transfer, each bounded by `USB_TIMEOUT_NS`, 2 s. The port
machine's enumeration and HID recovery submit and return; what still spins is
the boot path's bring-up, the stop, a disk's bind after boot, inside a
scheduler pass
(`issues/a-disk-plugged-in-after-boot-is-bound-inside-a-scheduling-pass.md`),
and a disk call.

A call on a USB disk holds `XHCI` from its first command (`with_disk`,
`kernel/src/drivers/xhci/wait/msc.rs`) inside a syscall, which runs with
preemption off from entry to exit and, since the gate opens them, interrupts
on (`issues/syscall-preemption-is-incidental.md`): a partition claim's
`SYS_PARTITION_READ`, `SYS_PARTITION_WRITE` and `SYS_FSYNC`, the last a cache
flush through `xhci::storage_flush` (`partition_fsync`,
`kernel/src/object/ops.rs`), and the table `SYS_DEVICE_CLAIM` reads for one
(`gpt::claimable`, `kernel/src/gpt.rs`). logd's `fsync` is one of these: the
LOG fsd answers each with `SYS_FSYNC` on its claim. So its CPU holds
preemption off for as long as the device takes. A disk's bind after boot
spins inside the Ring 3 tick's pass, which runs with interrupts masked, and
`time::DEAF_CPU` (5 s), past which a CPU waiting on a TLB acknowledgement
panics, is held above `CALL_AFTER_BREAK` (4.75 s), the longest one call spins
once its transport has broken.

Its "On the T14" readings below were taken while a syscall still ran with
interrupts masked from entry to exit.

## On the T14

On LENOVO 20W0003AMZ, BIOS N34ET71W (1.71), `/log` is a FAT32 partition of the
USB stick the machine boots from, which `/system/bin/fsd` in the LOG role
writes with `SYS_PARTITION_WRITE`. One boot of `main` at `c59e09ed6` with a
throwaway instrument that records every window of 2 ms or more with the
addresses that opened and closed it, its CPU's last events, and samples of
that CPU every 2 ms ran `test_rs_ring_park_herd` forty times. Its lines are
quoted on #681 (comments 5968784527, 5962618492 and 5962618868). Pid 4 is that
fsd.

- **Eleven writes of 7.8 ms or more in 37 s**, 2.0 to 4.6 s apart, each window
  opened at the syscall's entry and closed at its return. Nine are on one CPU
  alone: 8,289,211 to 10,669,042 ns. 36 of their 37 samples are in
  `wait_transfer < bulk < bot < scsi < transfer_blocks` under `storage_write`;
  the 37th is the same syscall waiting for the process table's lock.
- **All eight CPUs at once, twice**, at 17.416 s and 21.116 s: the write on one
  CPU (7,848,573 and 7,927,725 ns) while a herd thread on each of the other
  seven is in `SYS_THREAD_EXIT`, whose unmap shoots down every CPU's TLB. The
  fourteen shootdowns span 7,703,612 to 7,852,515 ns, and their initiators
  spin: none goes more than 10,819 ns between two turns of its wait. The other
  targets acknowledge within 24 µs; the writing CPU acknowledges by interrupt
  once its syscall has returned, 130 to 264 ns before each shootdown ends. Each
  event is followed at once by a shorter one of the same shape: the next
  write, 2,824,878 and 2,846,782 ns, and fourteen more shootdowns. The
  report's `tlb:` line reads `max=7846us` after the first, where it read
  `max=1146us` before it.
- **Shorter**: sixteen of the same server's writes read 2,368,371 to
  2,905,716 ns, those two among them, and two of its `SYS_PARTITION_READ`
  2,596,003 and 2,954,780. At the stop, the black box keeps a write opened at
  38.795 s whose CPU's next event comes 7,953,409 ns later.
- **init's partition claims**: on cpu0, 6,002,063 ns at 1.171 s and
  6,623,907 ns at 1.179 s, each `SYS_DEVICE_CLAIM` from entry to return, all
  four samples in `wait_transfer` under `storage_read`.
- **Its `SYS_FSYNC`**: 15 windows' trails carry one, and in 7 it spins on a
  contended `XHCI` at `with_disk_by` (comment 5966523482). None held
  interrupts off for 2 ms.
- **Not the firmware**: `MSR_SMI_COUNT` does not move across any of them. The
  windows it does move across are
  `issues/the-t14s-firmware-interrupts-every-cpu-every-2-2-s-under-toyos.md`'s.

The `mask-windows` kernel (`kernel/src/windows.rs`) prints a span and no
opener, and #649's T14 boots printed these by reading:

- cpu0's line before the first job's exit, init's partition claims:
  6,540,326 to 6,590,095 ns in seven of the eight `mask_windows` boots at
  `72f16e39a`, and 6,533,222, 2 × 3,273,547 and 6,508,165 in the first report
  of the three at `8b73eba69` (comment 5959415453).
- Every CPU at once: 9,603,617 to 9,651,846 ns beside `tlb: … max=9575us`, the
  fourth boot at `72f16e39a`; 2 × 5,014,552 to 2 × 5,075,290 beside
  `max=10038us`, `649-r6/2-report-halved` (comment 5960575031).
- One CPU alone: 9,260,193, the seventh boot at `72f16e39a`; cpu7, where pid 4
  started, at 11,492,028 and 8,456,658 before the first job's exit
  (`649-r6/1-head`, `649-r6/3-idle-halt-counted`).
- The stop's report: one CPU at 10,214,820 to 11,267,384 in four boots at
  `72f16e39a`, and two in `649-r5/1-head`, cpu7 at 11,522,834 and cpu4 at
  11,645,411 (`loader.log:57`, `:63`).

**Owner**: `issues/the-kernel-is-small-interrupts-post-and-threads-wait.md`,
whose step 10 moves the whole xHCI to usbd and deletes the kernel's driver. It
owes both exits.

**Exit**, two:
- **The tripwire's, short of step 10**: no disk wait spins with `IF` clear,
  because the tick's pass leaves the interrupt gate (step 2 of
  `issues/toyos-beats-linuxs-latency-on-the-t14.md`), and
  `kernel/src/drivers/xhci/mod.rs` holds no bound under `DEAF_CPU`.
- **The spin's**: the tree has no `kernel/src/drivers/xhci`, so no kernel wait
  is a USB device's. Step 10 meets both.
