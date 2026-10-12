---
status: open
kind: defect
opened: 2026-10-12
---

# No claim gets an address space under virtio-iommu

A process is handed a PCI function only once the function has an address
space of its own at a unit and, on x86-64, interrupt remapping
(`kernel/src/pcidev/mod.rs`'s header; `Refusal::NotRemapped`,
`Refusal::Untranslated`). The units this kernel programs are VT-d (`DMAR`),
AMD-Vi (`IVRS`) and SMMUv3 (`IORT`) (`kernel/src/iommu/`). QEMU emulates all
three, and VirtualBox VT-d or AMD-Vi under `--iommu` on its ICH9 chipset, off
by default (`VBoxManage modifyvm`). Cloud Hypervisor and crosvm put their
devices behind virtio-iommu instead (virtio 1.2 §5.13, device id 23, PCI
`1af4:1057`): Cloud Hypervisor's documentation says it implements it "in order
to provide a virtual IOMMU to its users" and "will not try to implement a full
emulation of a physical IOMMU" (its `docs/iommu.md`), describes it to the
guest in an ACPI `VIOT` table (its `vmm/src/acpi.rs`), and offers an SMMUv3
only for a passthrough device behind a physical one (`docs/iommu.md`,
"Emulated ARM SMMUv3"). So under either, every claim is refused and no
userland driver runs.

The two architectures differ in what virtio-iommu leaves undone:

- **AArch64.** A claimed function's message is its slot's LPI through the
  ITS (`kernel/src/arch/aarch64/irqchip/its.rs`), whose translation by
  DeviceID is the per-function boundary; virtio-iommu need only translate DMA.
  Cloud Hypervisor's AArch64 machine has an ITS (its `IORT`'s ITS group).
- **x86-64.** Virtio 1.2 defines no interrupt remapping: an MSI doorbell is a
  reserved region the driver may not map (§5.13.6.8.1,
  `VIRTIO_IOMMU_RESV_MEM_T_MSI`), whose writes pass untranslated. Nothing mints
  a `Remapping` there, so a claim still ends at `NotRemapped`. Whether a claim
  may stand under virtio-iommu with its messages unremapped is the owner's to
  rule: `issues/every-driver-is-still-in-the-kernel.md` records his ruling that
  "the IOMMU lands first and completely, interrupt remapping included".

bhyve offers its guest no IOMMU of any kind;
`issues/a-machine-without-an-iommu-refuses-every-claim.md` holds that case and
the owner's ruling on it.

**Owner**: whoever holds `issues/toyos-runs-in-any-open-source-hypervisor.md`.

**Exit**: on AArch64, a claim under virtio-iommu is handed over in a domain
of its own and a DMA outside its grants is a `DMA FAULT` record, under QEMU's
`virt` with `virtio-iommu-pci` and its emulated ITS; on x86-64, the owner's
ruling recorded here, and the claim refused by a `Refusal` that names
virtio-iommu's missing interrupt remapping or handed over as he rules.
