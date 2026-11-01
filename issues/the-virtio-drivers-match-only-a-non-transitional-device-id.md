---
status: open
kind: defect
opened: 2026-10-12
---

# The virtio drivers match only a non-transitional device id

Virtio 1.2 §4.1.2.2: "Drivers for device types listed in section 4.1.2 MUST
match devices with the PCI Vendor ID 0x1AF4 and the Transitional PCI Device ID
indicated in section 4.1.2" — `0x1000` for a network card, `0x1003` for a
console, among others — beside the `0x1040`-based id. A transitional device
also publishes the virtio PCI capabilities (§4.1.4), and §4.1.5.1.1.1 has a
non-transitional driver look for them and "fail gracefully" where they are
absent. That is not the legacy interface, which this tree refuses.

The tree matches the `0x1040`-based id alone:

- netstack's `CARDS` (`system/netstack/src/main.rs`) and its row in
  `images/system.toml`, `pci:1af4:1041`;
- virtio-console's row in `images/system.toml`, `pci:07:80:00/1af4:1043`,
  and the matcher its claim is endowed under.

The launcher and the harness hide it: each asks QEMU for
`virtio-net-pci-non-transitional` and `virtio-serial-pci-non-transitional`
(`build/qemu.rs`, `tests/common/qemu.rs`). QEMU's own default is transitional
on q35's root bus — `disable-legacy=auto` resolves to off for a device not
behind a PCIe port — and VirtualBox's virtio-net is `1af4:1000`. On either,
netstack has no card and the machine no virtio console. Cloud Hypervisor and
crosvm use the `0x1040`-based ids. bhyve's virtio-net and virtio-blk are
legacy-only, an I/O BAR and no virtio capability, so a conforming driver
matches them and refuses them by name.
Virtio-gpu and virtio-sound have no transitional id, so their matches are
already whole.

A transitional device offers no `VIRTIO_F_ACCESS_PLATFORM` on either: QEMU
refuses `iommu_platform=on` on a transitional device, and VirtualBox's virtio
devices offer the bit nowhere. So behind a unit such a NIC is one the
supervisor refuses every holder (`translated` in `system/supervisor/src/main.rs`);
the matches reach a card only on the path
`issues/a-machine-without-an-iommu-refuses-every-claim.md` rules for a machine
with no unit.

**Owner**: whoever holds `issues/toyos-runs-in-any-open-source-hypervisor.md`.

**Exit**: netstack's row, its `CARDS` and the two virtio-serial rows each name
both ids of their type; virtio-console carries a guest boot whose console is
QEMU's transitional `virtio-serial-pci`, and refuses the same device under
`disable-modern=on` by name.
