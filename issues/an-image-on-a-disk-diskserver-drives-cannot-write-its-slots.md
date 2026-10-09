---
status: open
kind: defect
opened: 2026-09-27
---

# An image on a disk diskserver drives cannot write its slots

The updater writes the idle slot through a partition claim
(`SYS_DEVICE_CLAIM` with `DeviceType::Partition`, granted by `system.toml`'s
`slots` row), and the kernel answers a claim only on the disks it drives: the
USB disks. The kernel drives no NVMe controller: `diskserver` does
(`userland/diskserver`). So a machine booted from an image on its NVMe disk —
every guest whose profile's storage is `Storage::Disk`
(`tests/common/qemu.rs`), and the T14 once ToyOS is installed on its internal
disk — finds its slots nowhere a claim reaches, and an update is refused at
the claim: `slot_grant` (`userland/supervisor/src/main.rs`) asks the inventory
for the ROOT partition the kernel holds, and the kernel lists no partition of
a disk it does not drive. Read off that function, and not run: the tree holds
no test that runs an update on any machine, which is why nothing fails.

The launcher (`src/qemu.rs`) boots every machine it starts off a USB stick
for this reason: a machine that updates itself keeps its image where a claim
reaches its slots, and moves onto its NVMe disk only once this issue's exit
is met.

**Exit**: the slots of an image on a disk `diskserver` drives are written
through a session the supervisor grants the updater on `diskserver`'s port,
judged by an update test that boots a profile whose storage is
`Storage::Disk`.

## Owner

`userland/supervisor` and `userland/diskserver`; unheld.
