---
status: open
kind: track
opened: 2026-09-29
---

# ToyOS runs on the Raspberry Pi

Owner ruling, 2026-09-29: ToyOS is general-purpose hardware, not centred on
the T14, and a machine with no IOMMU is supported rather than excluded — the
Raspberry Pi is a wanted target for exactly that case. On such a machine only
a driver signed and shipped in the ToyOS image may claim a device, never a
user-installed one; the drivers are the same code as on an IOMMU machine, and
the difference is confined to the kernel's claim path
(`issues/kernel/a-machine-without-an-iommu-refuses-every-claim.md`), never
duplicated into a driver. This is a specific target underneath
`issues/kernel/toyos-runs-on-arm64.md`, whose ACPI-only, EL1-only and staged
seam rulings apply here unchanged; nothing below repeats them, and hardware
specifics (which SoC and board revision, which UEFI build) are for whoever
picks this up to check against current sources rather than fixed here.

Staged work, each stage its own exit:

1. **Boot media.** An SD card image carrying the Pi's own boot files plus a
   community EDK2 UEFI build, with no flashing step — the card is written
   once and the loader takes over from UEFI exactly as it does on the T14.
   **Needs the owner's decision first**: whether the Pi's proprietary boot
   files fit CLAUDE.md's vendor-firmware rule (redistributable, pinned,
   recorded in `NOTICE`) or need a separate ruling — they are not loaded by a
   ToyOS driver through a device's own signature check the way that rule
   assumes. Exit: the card boots the UEFI shell with no host-side flashing
   tool.

2. **A GICv2 interrupt controller.** The Pi's GIC is GICv2, not the GICv3
   `toyos-runs-on-arm64.md`'s QEMU-`virt` stage targets: distributor and CPU
   interface only, no redistributors, no ITS, SGIs as the IPI. Exit: a user
   process takes an interrupt-driven syscall and a timer tick on the Pi under
   the same GIC seam the GICv3 arm uses.

3. **No-IOMMU DMA and the signed-claim rule.** The Pi has no IOMMU unit at
   all — the kernel's first machine to exercise `DeviceSpace::Untranslated`
   outside a test, and the signed-in-image claim path built for
   `a-machine-without-an-iommu-refuses-every-claim.md`. Exit: a claim from a
   signed in-image driver succeeds and receives a physical address; a claim
   attempted by anything else is refused by name.

4. **The Pi's own device drivers, in userland.** An SD/MMC host controller
   driver for root and boot storage and an Ethernet MAC driver for the
   answer path, both built the way `userland/netd/src/virtio_net.rs` and the
   T14's I219 driver are: claimed through the kernel's device-claim path,
   never compiled into the kernel. Exit: the Pi boots from its own SD card,
   reaches the network under DHCP, and runs the same userland image the T14
   does.
