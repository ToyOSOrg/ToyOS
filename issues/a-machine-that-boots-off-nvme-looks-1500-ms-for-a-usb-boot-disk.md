---
status: open
kind: defect
opened: 2026-10-09
---

# A machine that boots off NVMe looks 1500 ms for a USB boot disk

`gpt::probe_usb_disks` (`kernel/src/gpt.rs`) keeps asking the USB disks for
the partition firmware booted from until one carries it or
`xhci::PORT_SETTLE_CEILING`, 1500 ms, has passed. On a machine whose image is
on its NVMe disk no USB disk ever carries it, so every boot spends the whole
ceiling, on the boot CPU, before a task runs: paced by `PORT_POLL` in a
`spin_loop`, with nothing else scheduled.

Measured on `nvme_disk_keeps_log_and_home`'s machine, which has no USB
controller at all, on both of its boots: the kernel says `xHCI: no controller
on this machine, USB input unavailable` and, stamped 1501 ms later (`2.917`
and `4.418` on the test's first run), `usb-storage: 0 disk(s) on this machine
and none carries the boot partition after 1500 ms of looking`. Those boots
report `kernel to Boot: complete 1741 ms`. And on `bar_map_again`'s, a
`Headless` machine whose xHCI carries a keyboard and no disk: `usb-storage: 0
device(s)` at `6.126` and the same line at `7.628`.

The kernel already knows the outcome it waits for is impossible on a machine
with no xHCI controller, and it is told nothing about which bus the boot
partition's disk is on: the loader has the device path and hands over the
partition's GUID and extent alone.

**Exit condition.** A machine that boots off a disk this kernel does not
drive spends none of that ceiling: the line above is gone from
`nvme_disk_keeps_log_and_home`'s boots, or the wait is deleted with the
kernel's xHCI at step 10 of
`issues/the-kernel-is-small-interrupts-post-and-threads-wait.md`.

## Owner

`kernel/src/gpt.rs`; unheld.
