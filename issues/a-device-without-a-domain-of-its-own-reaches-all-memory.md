---
status: open
kind: defect
opened: 2026-09-29
---

# A device without a domain of its own reaches all memory

Every enumerated PCI function is bound to one identity domain over
`[0, pmm::top())` (`kernel/src/arch/x86_64/vtd/mod.rs`, `table::identity_domain` and
`identity_root`), and only
a driver that asks gets an address space of its own
(`kernel/src/iommu/mod.rs`'s header). A device the kernel drives, or one nothing
drives, can read and write every page by DMA. Linux at `Ubuntu-6.8.0-142.142`,
under the T14's `CONFIG_INTEL_IOMMU_DEFAULT_ON=y` and
`CONFIG_IOMMU_DEFAULT_DMA_LAZY=y`, turns DMA remapping on
(`drivers/iommu/intel/iommu.c:227`) and gives every device a translated
default domain (`drivers/iommu/iommu.c:197-210`), which maps only what its
driver maps.

A claimed function leaves the identity domain for one of its own. Where an RMRR names a display or USB controller alone, its range is mapped in that domain at its own address (`vtd::classify`, `domain::keep_reserved`), by the owner's ruling of 2026-10-04 ("Display and USB only", `issues/the-iommu-refuses-nothing-yet.md`). On the T14 this is `tgl-display`'s claim of the iGPU, which keeps the panel lit.

**Exit**: a function's DMA reaches only what its driver mapped and the reserved
ranges firmware names for it; a `boot-actuators` arm that points an unclaimed
function's DMA at a kernel page records a translation fault, and binding it to
the identity domain reds it.
