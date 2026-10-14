---
status: open
kind: tooling
opened: 2026-10-12
---

# QEMU's `INVALIDATE_IOMMU_ALL` leaves every function untranslated

AMD 48882 §2.4.8: once `INVALIDATE_IOMMU_ALL` completes, "all IOMMU caches
are empty", so the unit reads every device table entry afresh. QEMU 11.1.1's
`amd-iommu` does not. With `dma-remap=on` a function's address space starts
on `iommu_nodma`, which passes its DMA untranslated, and moves to
translation only in `amdvi_update_addr_translation_mode`, whose one caller is
`amdvi_inval_devtab_entry`. `amdvi_inval_all` resets the IOTLB, notifies the
interrupt caches and replays the address spaces that have notifiers, and
never reads an entry's mode (`hw/i386/amd_iommu.c` at `c3d48b7d`,
`:1249-1313` and `:1332-1351`).

So under `iommu-firmware-left`, which invalidates with one
`INVALIDATE_IOMMU_ALL` (`kernel/src/arch/x86_64/amdvi/mod.rs`, `leave_on`),
QEMU passes every function's DMA through a unit whose every entry blocks it.
`amdvi_firmware_left`'s second boot binds the stick whether or not the
switch-off runs: its negative control reds only on the register half, and the
DMA half is held by the AMD bench's `iommu_firmware_left` row alone, in
whatever run the orchestrator stages.

Owner: the orchestrator's AMD laptop track, whose translation code
(`kernel/src/arch/x86_64/amdvi/`) rests on what QEMU does on this command.

**Exit:** under the QEMU `build/ci/qemu-version` names, `amdvi_firmware_left`
with `switch_off` skipping every field reds on `usb-storage: 1 device(s)`,
and the test's doc says that boot's stick never binds.
