---
status: open
kind: track
opened: 2026-09-29
---

# ToyOS runs on the Raspberry Pi

**Parked.** Blocked on the ARM exit (`issues/kernel/toyos-runs-on-arm64.md`,
"Exit"), on the owner buying a Pi — no ARM hardware is bought now — and on
`issues/hardware/may-the-raspberry-pi-image-carry-its-boot-firmware-and-a-c-built-u-boot.md`.
Nothing here is work until all three clear.

**Owner ruling, 2026-09-29: the boot route is U-Boot.** The Pi's own firmware
loads U-Boot from the SD card, with no flashing step; U-Boot provides UEFI
per Arm's EBBR and passes a devicetree. ToyOS stays UEFI-only, with one boot
path: the devicetree is hardware description, read through
`issues/kernel/the-kernel-reads-hardware-from-a-devicetree.md`. The Pi has no
IOMMU, so it runs under
`issues/kernel/a-machine-without-an-iommu-refuses-every-claim.md`.

**Recorded weakness**: U-Boot's UEFI variables are file-backed on the SD
card, so the anti-rollback floor there is only as strong as physical control
of the card. It stays a weakness until the floor lives in storage that
rewriting or swapping the card cannot reset.

What is to be built: an SD card image carrying the Pi's boot files, U-Boot
and its devicetree, and the ToyOS loader; a GICv2 interrupt controller; and,
in userland, an SD host controller driver and a GENET Ethernet driver for the
Pi 4.

Exit: the image's loader and kernel boot under
`qemu-system-aarch64 -M virt,gic-version=2` with no SMMU, through U-Boot's
EBBR UEFI, on the devicetree U-Boot passes, and the kernel says "no isolation
on this machine". The board's own parts — its boot files, the SD host
controller, GENET — are not this exit: they are added to it when the owner
buys a Pi.
