---
status: open
kind: defect
opened: 2026-10-02
---

# A log write to the T14's stick masks interrupts for its whole USB transfer, on one CPU or on all eight

On LENOVO 20W0003AMZ, BIOS N34ET71W (1.71), `/log` is a FAT32 partition of the
USB stick the machine boots from, and `/system/bin/fsd` in the LOG role writes
it with `SYS_PARTITION_WRITE`, which the kernel's USB mass storage serves. The
kernel waits for the transfer by spinning on the event ring under the
controller's lock (`XhciController::wait_transfer`,
`kernel/src/drivers/xhci/wait/mod.rs`), inside a syscall, which runs with
interrupts masked from entry to exit. So the writing CPU holds interrupts and
preemption off for as long as the stick takes, and a CPU whose shootdown waits
on it waits as long, masked.

Read in one T14 boot of `main` at `c59e09ed6` with a throwaway instrument that
records every window of 2 ms or more with the addresses that opened and closed
it, its CPU's last events, and samples of that CPU every 2 ms; the boot ran
`test_rs_ring_park_herd` forty times. Its lines are quoted on #681 (comments
5962618149, 5962618492 and 5962618868). Pid 4 is that fsd.

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
  once its syscall has returned, 74 to 299 ns before each shootdown ends. Each
  event is followed at once by a shorter one of the same shape: the next
  write, 2,824,878 and 2,846,782 ns, and fourteen more shootdowns. The
  report's `tlb:` line reads `max=7846us` after the first, where it read
  `max=1146us` before it.
- **Shorter**: sixteen of the same server's writes read 2,368,371 to
  2,905,716 ns, those two among them, and two of its
  `SYS_PARTITION_READ` 2,596,003 and 2,954,780. At the stop, the black box
  keeps a write opened at 38.795 s whose CPU's next event comes
  7,953,409 ns later.
- **Not the firmware**: `MSR_SMI_COUNT` does not move across any of them. The
  windows it does move across are
  `issues/hardware/the-t14s-firmware-interrupts-every-cpu-every-2-2-s-under-toyos-and-not-under-linux.md`'s.
- **The instrument's control**: a 50,000,024 ns hold reads back as a
  56,000,902 ns window on its CPU, sampled in `kernel::windows::hold_once`
  every 2 ms. The excess is the instrument's own report, which that same
  `SYS_EXIT` prints (5,918,088 ns): a window on a CPU printing a report
  includes the report.

The `mask-windows` kernel (`kernel/src/windows.rs`) prints a span and no
opener. #649's fourteen T14 boots printed spans that are these writes by
reading, no opener having been recorded: every CPU at once at 9,603,617 to
9,651,846 ns beside `tlb: … max=9575us` (the fourth `mask_windows` boot at
`72f16e39a`) and at 2 × 5,014,552 to 2 × 5,075,290 beside `max=10038us`
(`649-r6/2-report-halved`, comment 5960575031); one CPU alone at 9,260,193
(the seventh at `72f16e39a`); cpu7, where pid 4 started in both boots, at
11,492,028 and 8,456,658 before the first job's exit (`649-r6/1-head`,
`649-r6/3-idle-halt-counted`); and one CPU at 10,214,820 to 11,645,411 in the
stop's report of five boots.

The window goes when the kernel stops serving the stick:
`issues/kernel/the-kernel-is-small-interrupts-post-and-threads-wait.md`'s
step 10 moves the xHCI to usbd, and its stage 5 exits on no interrupts-off
window longer than a register access. The waiters are masked because a
syscall is (`issues/kernel/syscall-preemption-is-incidental.md`).

**Exit**: a T14 boot that writes the log as this one did prints no
interrupts-off window that a USB transfer opens, read by an instrument that
names a window's opener; or this file is replaced by the bound the write is
held to and the derivation of it.
