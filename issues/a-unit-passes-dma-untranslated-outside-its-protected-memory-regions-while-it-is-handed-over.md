---
status: open
kind: defect
opened: 2026-10-04
---

# A unit passes DMA untranslated outside its protected memory regions while it is handed over

`vtd::init` (`kernel/src/arch/x86_64/vtd/mod.rs`) hands every unit over with
translation off. That covers a unit firmware left translating, whatever its
table mode. VT-d Rev. 4.1 gives no condition under which system software may
switch firmware's tables for its own under translation. §6.6 requires
identical remapping results, and nothing documented lets this kernel know
firmware's mappings. `DMA_CTRL_PLATFORM_OPT_IN` (§8.1) says only where
platform DMA goes.

The hand-over is guarded where every described unit is programmed, has both
protected memory regions (`CAP.PLMR`, `CAP.PHMR`) and takes register-based
invalidation. On such units the regions block DMA to what they cover whether
or not the unit translates (§11.4.8.1). They are programmed on every unit
after any firmware left on is switched off, and released once the last unit
translates (`vtd/pmr.rs`). The low region and the high region are each the
largest stretch of the OS's RAM (usable or ACPI-reclaimable, never NVS) that
avoids every RMRR and holds every 2 MiB page the hand-over's tables were
carved from, and this kernel's image. The boot logs the regions, what they
cover and the functions mastering the bus as they go on. The T14's four units
are guarded. The queue and interrupt remapping are programmed after the
release.

What passes untranslated during the hand-over:

- **A machine whose hand-over is unguarded:** all of memory. Its log says why:
  - a unit without both regions (QEMU's);
  - a unit that refuses register-based invalidation (major version 6 or later,
    §11.4.6.1), or whose IOTLB registers lie outside the window this kernel
    maps;
  - fewer units programmed than described;
  - tables or kernel outside every region.

  Such a machine invalidates through the queue, enabled before translation
  comes on.
- **A guarded machine:**
  - RAM outside the two chosen stretches;
  - every RMRR;
  - peer MMIO.
- **Before the hand-over, on any machine:**
  - Where firmware leaves translation off, as the T14's does, every function
    reaches all of memory from ExitBootServices until the regions go on.
  - Where firmware leaves it on, `mm::init` has already taken
    `EfiBootServicesData` as free (`lib/bootmap/src/lib.rs`,
    `is_usable_type`). A unit may walk firmware's tables in memory the pmm has
    handed out.

Evidence:
- No machine here is handed a unit translating.
- `iommu-firmware-left` stages one:
  - QEMU 11.1.1 runs the unguarded hand-over (`vtd_firmware_left`);
  - the T14 runs the guarded one (`iommu_firmware_left`'s row, which reads
    the regions go on before translation first goes off and come off before
    the first unit is programmed).

Owner: the IOMMU track (`issues/the-iommu-refuses-nothing-yet.md`).

**Exit**: during the hand-over no function reaches memory outside its RMRRs.
Each of these cases is closed by a documented mechanism and read on a
machine or a host test:

1. a unit without protected memory regions. §11.4.8.1 marks the regions
   "planned for deprecation", so new units will be this case. Abort-DMA mode
   (§3.4.4) is one mechanism; it needs `ECAP.ADMS` and is ruled out while a
   device an RMRR names masters the bus;
2. a unit that refuses register-based invalidation;
3. RAM a guarded hand-over leaves outside its regions;
4. peer MMIO.
