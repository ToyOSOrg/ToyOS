---
status: open
kind: track
opened: 2026-09-29
---

# ToyOS runs on the Raspberry Pi

Owner ruling, 2026-09-29: ToyOS is a general-purpose OS for modern hardware;
the T14 may decide the feature set but is not the design centre, and a
machine with no IOMMU is supported rather than excluded — the Raspberry Pi is
a wanted target for exactly that case. On such a machine only a driver signed
and shipped in the ToyOS image may claim a device, never a user-installed
one; the drivers are the same code as on an IOMMU machine, and the
difference is confined to the kernel's claim path
(`issues/kernel/a-machine-without-an-iommu-refuses-every-claim.md`), never
duplicated into a driver. Without an IOMMU a device's DMA is not contained,
so a bad signed driver can still corrupt the kernel; the full isolation
guarantee needs an IOMMU, and ToyOS states "no isolation" plainly on the Pi —
isolation tests there report "no isolation on this machine", never a pass.

This is a specific target underneath `issues/kernel/toyos-runs-on-arm64.md`,
whose EL1-only and staged seam rulings apply here unchanged; nothing below
repeats them, and hardware specifics (board revision, U-Boot version) are for
whoever picks this up to check against current sources rather than fixed
here.

**Owner ruling, 2026-09-29: the boot route is U-Boot, not community EDK2.**
The Pi's own closed firmware loads U-Boot from the SD card; U-Boot itself is
open source and ships as a file on the card, with no flashing step. U-Boot
provides UEFI per Arm's EBBR (the SystemReady Devicetree band) and passes a
devicetree to the OS. ToyOS stays UEFI-only, with one boot path — the
devicetree is read as hardware description, not as an alternative way to
boot, through `issues/kernel/the-kernel-reads-hardware-from-a-devicetree.md`.
**Recorded weakness**: U-Boot's UEFI variables are file-backed on the SD
card, so the anti-rollback floor on this machine is only as strong as
physical control of the card.

Staged work, each stage its own exit:

1. **Boot media.** An SD card image carrying the Pi's own boot files, U-Boot
   built for EBBR UEFI, and the ToyOS loader, written once with no flashing
   step. **Check before starting**: whether shipping the Pi's own boot files
   (Broadcom's closed GPU firmware, which the SoC's on-chip boot ROM runs
   before U-Boot or any ToyOS driver exists) fits CLAUDE.md's vendor-firmware
   rule — that rule assumes firmware loaded by the device's own driver
   through its IOMMU domain, and nothing here is loaded by a driver or
   IOMMU-mapped, so the shape does not obviously fit and this may need its
   own ruling. Exit: the card boots U-Boot's UEFI shell with no host-side
   flashing tool.

2. **Devicetree.** The kernel reads the memory map, interrupt controller and
   UART from the devicetree U-Boot passes, per
   `issues/kernel/the-kernel-reads-hardware-from-a-devicetree.md`. Exit: the
   kernel decodes the Pi's devicetree instead of a hardcoded MMIO map.

3. **A GICv2 interrupt controller.** The Pi's GIC is GICv2, not the GICv3
   `toyos-runs-on-arm64.md`'s QEMU-`virt` stage targets: distributor and CPU
   interface only, no redistributors, no ITS, SGIs as the IPI. Exit: a user
   process takes an interrupt-driven syscall and a timer tick on the Pi under
   the same GIC seam the GICv3 arm uses.

4. **No-IOMMU DMA and the signed-claim rule.** The Pi has no IOMMU unit at
   all — the kernel's first machine to exercise `DeviceSpace::Untranslated`
   outside a test, and the signed-in-image claim path built for
   `a-machine-without-an-iommu-refuses-every-claim.md`. Exit: a claim from a
   signed in-image driver succeeds and receives a physical address; a claim
   attempted by anything else is refused by name; an isolation test on the
   Pi reports "no isolation on this machine", never a pass.

5. **The Pi's own device drivers, in userland.** An SD host controller
   driver for root and boot storage and an Ethernet MAC driver for the
   answer path, both built the way `userland/netd/src/virtio_net.rs` and the
   T14's I219 driver are: claimed through the kernel's device-claim path,
   never compiled into the kernel. The Pi 4 is the first target, with GENET
   as its Ethernet MAC; the Pi 5 follows, whose Ethernet sits behind the RP1
   southbridge instead of on the SoC directly. Exit: the Pi 4 boots from its
   own SD card, reaches the network under DHCP, and runs the same userland
   image the T14 does.
