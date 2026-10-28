---
status: open
kind: defect
opened: 2026-10-04
---

# A unit left translating passes DMA untranslated while it is programmed

`vtd::enable` (`kernel/src/arch/x86_64/vtd/mod.rs`) switches off the
translation firmware left on (`Unit::hand_over`) once the unit's tables are
built, unless firmware declares what it has in flight (below), and turns it
back on through this kernel's root table only after the queue, fault
reporting, both table pointers and a global invalidation are programmed. In
between, every function behind that unit reaches all of memory.

Keeping `TE` on is what VT-d Rev. 4.1 §6.6 allows only onto tables that remap
in-flight requests exactly as the ones being walked. Where firmware sets
`DMA_CTRL_PLATFORM_OPT_IN` (§8.1), its DMA reaches only its RMRRs, which
§3.16 has mapped identity and read-write. If the identity domain holds every
RMRR and every device they name is enumerated, this kernel's root translates
them alike, and the hand-over keeps `TE` on. The T14 is such a machine.
Elsewhere, firmware's tables have to be read and copied first, as Linux's
kdump path does (`copy_translation_tables`, `drivers/iommu/intel/iommu.c`);
Linux outside kdump switches `TE` off as this kernel does. Any copy has to
read them before the pmm hands their memory out: it takes
`EfiBootServicesData` as free (`lib/bootmap/src/lib.rs`, `is_usable_type`).

Evidence: no machine here is handed a unit translating; the T14's four units
read `gsts=0x40000000` (pull request #700's selftests boot). QEMU's DMAR leaves
`DMA_CTRL_PLATFORM_OPT_IN` clear (`dma_ctrl_opt_in=n`). Staged by the
`iommu-firmware-left` actuator on QEMU 11.1.1, unit0 logs `was handed over
with translation on` at 0.083 s, just before translation goes off, and
`translating` at 0.090 s, just after it is back; two of the hand-over's own log
lines fall between.

One case keeps the window whatever this kernel does: a unit handed over with
`TTM` not `00` and `CAP.ESRTPS` clear, where §6.6 forbids switching tables
under translation, so `TE` has to go off. **Owner ruling, 2026-10-04: accept
it and file it.** The fix that closes this issue logs that case and files it
as its own defect, whose exit covers the window with protected memory regions
(`PMEN` over every pmm frame) or has the kernel speak scalable mode.
No machine here reaches it.

**Exit**: under `iommu-firmware-left`, on QEMU as on the T14, no unit logs
`was handed over with translation on; it goes off first`, and every unit's
`translating` line reads `tes=y`.
