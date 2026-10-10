---
status: open
kind: defect
opened: 2026-10-10
---

# An ECAM window past the CPU's physical address width is mapped

`acpi::ecam_windows` (`kernel/src/drivers/acpi.rs`) bounds each MCFG window by
`toyos_bootmap::DIRECT_MAP_WINDOW` (128 TiB), the first address the direct
map cannot hold, and not by the physical address width the CPU implements. A
window an MCFG puts between the two is mapped: on x86-64 the page-table entry
then sets bits past `MAXPHYADDR`, which are reserved, and the scan's first
read of it is a page fault in the kernel, which panics on firmware input.

**Evidence:** the code. Every MCFG this tree has read puts its window below
4 GiB.

Owner: orchestrator. Exit: the limit `ecam_windows` passes to
`EcamWindow::mappable` is the lower of `DIRECT_MAP_WINDOW` and the width the
CPU reports (CPUID leaf `0x8000_0008` `EAX[7:0]` on x86-64,
`ID_AA64MMFR0_EL1.PARange` on AArch64), so such a window is refused
`PastLimit` by name.
