---
status: open
kind: defect
opened: 2026-09-26
---

# The running slot's FAT volume is claimable, where its ROOT is not

The kernel holds the partition its ROOT image came from for the machine's life
(`kernel/src/rootfs.rs`, `hold_source`), so no claim can write the ROOT a boot
runs. It holds nothing of the same slot's FAT volume — the kernel, its boot
parameter and the signed header the loader read — so on a disk the kernel
drives, any process whose `SysCap` carries `device` can claim that partition
by its unique GUID and write it. The test estate's `test-runner` holds
`device`, and so would any row a config gives it to.

`/system/bin/update` does not: the loader names the booted volume in the
inventory (`Role::Slot`), and `slots::grant` refuses an idle slot naming it,
whatever the table the updater wrote says.

A write there is not a silent compromise: the loader holds every byte to the
signature at the next boot and refuses the slot by name. It is a denial —
the running slot is refused next boot and the machine falls back to an older
image, or to nothing on a one-slot image.

**Exit**: the kernel's USB storage path is deleted with its xHCI driver
(`issues/the-kernel-is-small-interrupts-post-and-threads-wait.md`, step 10).
The kernel then serves no partition claim, `hold_source` goes with it, and
the running slot's volume and ROOT are reached only through the block
service's grants (`issues/the-block-service-does-not-refuse-the-running-root.md`).
No kernel hold of the volume is added in the meantime.
