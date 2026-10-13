---
status: open
kind: defect
opened: 2026-10-12
---

# No claim gets an address space under virtio-iommu

A process is handed a PCI function only once the function has an address
space of its own at a unit and, on x86-64, interrupt remapping
(`kernel/src/pcidev/mod.rs`'s header; `Refusal::NotRemapped`,
`Refusal::Untranslated`). The units this kernel programs are VT-d (`DMAR`,
`kernel/src/arch/x86_64/vtd/`) and SMMUv3 (`IORT`,
`kernel/src/arch/aarch64/smmu/`). It finds AMD-Vi units (`IVRS`) and programs
none (`kernel/src/arch/x86_64/iommu_unit.rs`), so a machine whose unit is AMD-Vi,
VirtualBox's `--iommu amd` and QEMU's `amd-iommu` among them, refuses every
claim until `issues/an-amd-vi-machine-isolates-no-device.md` lands.

QEMU emulates VT-d, AMD-Vi and SMMUv3, and VirtualBox VT-d or AMD-Vi under
`--iommu` on its ICH9 chipset, off by default (`VBoxManage modifyvm`). Cloud
Hypervisor offers
virtio-iommu instead (virtio 1.2 §5.13, device id 23, PCI `1af4:1057`): its
documentation says it implements it "in order to provide a virtual IOMMU to its
users" and "will not try to implement a full emulation of a physical IOMMU",
and puts a device behind it only where the device is tagged `iommu=virtio`,
whose "default value will always be `off`" (its `docs/iommu.md`); it describes
the unit to the guest in a `VIOT` table, and offers an SMMUv3 only for a
passthrough device behind a physical one (`docs/iommu.md`, "Emulated ARM
SMMUv3"). crosvm puts only a host device passed through with
`--vfio …,iommu=viommu` behind its virtio-iommu, never a device it emulates.
So Cloud Hypervisor by default, and crosvm for every device it emulates, are
the no-unit case of
`issues/a-machine-without-an-iommu-refuses-every-claim.md`, and a device
Cloud Hypervisor is told to put behind its virtio-iommu is refused here too.

**Programming a virtio-iommu puts virtio in the kernel**, on either
architecture. Its requests travel on a virtqueue, its `requestq` (§5.13.2),
over the virtio PCI transport, and the unit a claim's domain lives at is the
kernel's (`pcidev`'s header). That conflicts with the owner's target that
"`virtio` does not appear anywhere in the kernel"
(`issues/every-driver-is-still-in-the-kernel.md`), and where a virtio-iommu
driver lives is the owner's to rule, for both architectures.

The two architectures differ in what virtio-iommu leaves undone besides:

- **AArch64.** A claimed function's message is its slot's LPI through the
  ITS (`kernel/src/arch/aarch64/irqchip/its.rs`), whose translation by
  DeviceID is the per-function boundary; virtio-iommu need only translate DMA.
  Cloud Hypervisor's AArch64 machine has an ITS (its `IORT`'s ITS group).
- **x86-64.** Virtio 1.2 defines no interrupt remapping: an MSI doorbell is a
  reserved region the driver may not map (§5.13.6.8.1,
  `VIRTIO_IOMMU_RESV_MEM_T_MSI`), whose writes pass untranslated. Nothing mints
  a `Remapping` there, so a claim still ends at `NotRemapped`. Whether a claim
  may stand under virtio-iommu with its messages unremapped is the owner's to
  rule too: `issues/every-driver-is-still-in-the-kernel.md` records his ruling
  that "the IOMMU lands first and completely, interrupt remapping included".

bhyve offers its guest no IOMMU of any kind;
`issues/a-machine-without-an-iommu-refuses-every-claim.md` holds that case and
the owner's ruling on it.

**Owner**: whoever holds `issues/toyos-runs-in-any-open-source-hypervisor.md`,
blocked on the owner's rulings above.

**Exit**: the owner's ruling on where a virtio-iommu driver lives, and on
x86-64 his ruling on unremapped messages, recorded here; and on each
architecture a claim under QEMU's `virtio-iommu-pci` (on AArch64 under `virt`
with its emulated ITS) handed over in a domain of its own, a DMA outside its
grants a `DMA FAULT` record, or refused by a `Refusal` that names
virtio-iommu, as he rules.
