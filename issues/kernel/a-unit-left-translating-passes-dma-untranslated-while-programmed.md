---
status: open
kind: defect
opened: 2026-10-04
---

# A unit left translating passes DMA untranslated while it is programmed

`vtd::enable` (`kernel/src/arch/x86_64/vtd/mod.rs`) switches off the
translation firmware left on (`Unit::hand_over`) once the unit's tables are
built, and turns it back on through this kernel's root table only after the
queue, fault reporting, both table pointers and a global invalidation are
programmed. In between, every function behind that unit reaches all of memory.

Keeping `TE` on instead is what VT-d Rev. 4.1 §6.6 allows only onto tables that
remap exactly as the ones being walked, which takes firmware's tables read and
copied first, as Linux's kdump path does (`copy_translation_tables`,
`drivers/iommu/intel/iommu.c`); Linux outside kdump switches `TE` off as this
kernel does. Any copy has to read them before the pmm hands their memory out:
it takes `EfiBootServicesData` as free (`toyos-bootmap/src/lib.rs`,
`is_usable_type`).

Evidence: no machine here is handed a unit translating; the T14's four units
read `gsts=0x40000000` (pull request #700's selftests boot). Staged by the
`iommu-firmware-left` actuator on QEMU 11.1.1, unit0 logs `was handed over
with translation on` at 0.083 s, just before translation goes off, and
`translating` at 0.090 s, just after it is back; two of the hand-over's own log
lines fall between.

**Exit**: under `iommu-firmware-left`, on QEMU and on the T14, no unit logs
`was handed over with translation on; it goes off first`, and every unit's
`translating` line reads `tes=y`.
