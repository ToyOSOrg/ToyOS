---
status: open
kind: defect
opened: 2026-09-29
---

# A device without a domain of its own reaches all memory

Every enumerated PCI function is bound to one identity domain over
`[0, pmm::top())` (`kernel/src/arch/x86_64/vtd/mod.rs:148,456-490`), and only
a driver that asks gets an address space of its own
(`kernel/src/iommu/mod.rs:3-5`). A device the kernel drives, or one nothing
drives, can read and write every page by DMA. Linux at `Ubuntu-6.8.0-142.142`,
under the T14's `CONFIG_INTEL_IOMMU_DEFAULT_ON=y` and
`CONFIG_IOMMU_DEFAULT_DMA_LAZY=y`, turns DMA remapping on
(`drivers/iommu/intel/iommu.c:227`) and gives every device a translated
default domain (`drivers/iommu/iommu.c:197-210`), which maps only what its
driver maps. A row of the
hardening table in
`issues/kernel/the-kernel-mitigates-what-linux-mitigates-on-the-t14.md`.

**Exit**: a function's DMA reaches only what its driver mapped and the reserved
ranges firmware names for it; a `boot-actuators` arm that points an unclaimed
function's DMA at a kernel page records a translation fault, and binding it to
the identity domain reds it.
