---
status: owner
kind: question
opened: 2026-09-29
---

# May the Raspberry Pi image carry its closed boot firmware and a C-built U-Boot?

`issues/kernel/toyos-runs-on-the-raspberry-pi.md` needs an SD card image
holding the Pi's own boot files and U-Boot. Neither fits a rule as written:

- **The vendor-firmware rule** admits firmware that a device verifies by its
  maker's signature, that its own driver loads through its IOMMU domain, and
  that never executes on the CPU. The Pi 4's boot files meet none of these as
  far as is known here: whether the SoC's boot ROM verifies them by signature
  on an unlocked board is unverified; the ROM loads them, not a ToyOS driver,
  and the Pi has no IOMMU; and the firmware brings an ARM stub that runs on
  the Cortex-A72 before U-Boot.
- **The firmware passes a binary devicetree**, compiled from sources outside
  ToyOS, which the image would carry as a committed binary.
- **U-Boot is third-party C.** Building it needs a C toolchain and make, which
  the dependency rule's "only Rust and QEMU" does not admit; being open source
  does not answer that.

Asked of the owner: may the image carry the Pi's boot files and its
devicetree blob, and on what terms; and may U-Boot be built from source in
this tree, or carried some other way?
