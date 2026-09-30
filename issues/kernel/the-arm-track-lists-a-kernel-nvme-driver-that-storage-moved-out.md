---
status: open
kind: tooling
opened: 2026-09-30
---

# The ARM track lists a kernel NVMe driver that storage moved out

`issues/kernel/toyos-runs-on-arm64.md`'s stage 4 says `arch::msi_message`
refuses "so the kernel's xHCI (`virt`'s boot stick), NVMe, HDA, virtio-sound,
virtio-console and virtio-gpu drivers each refuse their function by name".
#536 deleted `kernel/src/drivers/nvme.rs`: an NVMe controller is
`/system/bin/blockd`'s, a claimed function, and `kernel/src/drivers/pci.rs` is
`msi_message`'s one caller. The track's "NVMe, xHCI and virtio have no ISA
dependence" in its assessment names the same driver.

The exit is the track's lines saying what the tree holds: NVMe out of the
kernel's list, and the claimed function's MSI named where it is owed
(`issues/kernel/nothing-reaches-the-msi-arm-of-a-claimed-function.md`).
