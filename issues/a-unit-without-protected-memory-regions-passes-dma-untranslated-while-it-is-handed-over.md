---
status: open
kind: defect
opened: 2026-10-04
---

# A unit without protected memory regions passes DMA untranslated while it is handed over

`vtd::init` (`kernel/src/arch/x86_64/vtd/mod.rs`) hands every unit over with
translation off. That covers a unit firmware left translating, whatever its
table mode. VT-d Rev. 4.1 gives no condition under which system software may
switch firmware's tables for its own under translation. §6.6 requires
identical remapping results, and nothing documented lets this kernel know
firmware's mappings. `DMA_CTRL_PLATFORM_OPT_IN` (§8.1) restricts only where
platform DMA goes.

While translation is off, a unit's protected memory regions (§11.4.8) block
DMA to the memory they cover. Where every unit reports `CAP.PLMR` and
`CAP.PHMR`, as the T14's four do, `vtd/pmr.rs` programs one pair of regions
on all of them before the first goes off, and releases them once the last is
translating. The pair is the largest stretch of firmware's RAM below 4 GiB,
from 16 MiB, and the largest at or above it, each clear of every RMRR
(`kernel::dmaguard`). Register-based invalidation completes the hand-over
inside them, since the invalidation queue's memory is no remapping structure
(Appendix A).

What still passes untranslated:

- **On a unit without both regions, all of memory.** QEMU's unit is one. The
  boot logs `has no protected memory regions`.
- **On the T14, outside the pair:**
  - RAM below 16 MiB;
  - RAM in a stretch the map breaks with a reserved or runtime entry;
  - every RMRR, the iGPU's stolen memory among them;
  - peer MMIO.
- **Before the hand-over.** `mm::init` runs before `iommu::init` and takes
  `EfiBootServicesData` as free (`lib/bootmap/src/lib.rs`, `is_usable_type`).
  A unit left translating may walk firmware's tables in memory the pmm has
  already handed out.

A unit of major version 6 or later refuses register-based invalidation
(§11.4.6.1), so `plan` leaves it unprogrammed. No machine here has one.

Evidence:
- No machine here is handed a unit translating; the T14's four read
  `gsts=0x40000000`.
- `iommu-firmware-left` stages one. QEMU 11.1.1 runs the unguarded hand-over
  (`vtd_firmware_left`). The T14 runs the guarded one (`iommu_firmware_left`'s
  row, which reads the arm before translation goes off and the release after).

Owner: the IOMMU track (`issues/the-iommu-refuses-nothing-yet.md`).

**Exit**: no unit passes DMA to memory outside its RMRRs while it is handed
over.

- §11.4.8.1 marks the protected regions "planned for deprecation". A unit
  without them needs another documented mechanism. Abort-DMA mode (§3.4.4)
  is one: it needs `ECAP.ADMS`, which the T14 lacks, and §3.4.4 rules it out
  while a device an RMRR names is mastering the bus.
- A host test or a machine reads each case.
