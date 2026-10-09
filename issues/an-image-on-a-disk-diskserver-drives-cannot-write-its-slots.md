---
status: open
kind: defect
opened: 2026-09-27
---

# An image on a disk blockd drives cannot write its slots

The updater writes the idle slot through a partition claim
(`SYS_DEVICE_CLAIM` with `DeviceType::Partition`, granted by `system.toml`'s
`slots` row), and the kernel answers a claim only on the disks it drives —
the USB disks, until usbd drives the controller. The kernel drives no NVMe
controller: blockd does (`userland/blockd`). So a machine booted from an image
on its NVMe disk — every guest whose profile's storage is `Storage::Disk`
(`tests/common/qemu.rs`), the x86-64 machine `cargo run` boots
(`src/qemu.rs`'s `Medium::Disk`), and the T14 once ToyOS is installed on its
internal disk — finds its slots nowhere a claim reaches, and an update is
refused at the claim: `slot_grant` (`userland/supervisor/src/main.rs`) asks
the inventory for the ROOT partition the kernel holds, and the kernel lists
no partition of a disk it does not drive. Read off that function, and not
run: the tree holds no test that runs an update on any machine, which is why
nothing fails.

**Exit**: the slots of an image on a disk blockd drives are written through a
session init grants the updater on blockd's port, judged by an update test
that boots a profile whose storage is `Storage::Disk`.
