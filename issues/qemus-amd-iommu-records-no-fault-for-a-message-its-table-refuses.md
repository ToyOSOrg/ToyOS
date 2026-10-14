---
status: open
kind: tooling
opened: 2026-10-14
---

# QEMU's `amd-iommu` records no fault for a message its table refuses

AMD 48882 rev 3.11 §2.2.5.2, step 7: a message whose remapping entry has
`RemapEn` clear is target-aborted and reported as an `IO_PAGE_FAULT` with `I`
set (Table 57). QEMU 11.1.1's `amd-iommu` (`intremap=on,xtsup=on`, 128-bit
entries) drops such a message and writes no event record: in
`usbd_drives_the_spare_amdvi`, a claim's holder rewrote its MSI-X entry from
the claim's vector 0x42 to 0x43, usbd's No-Op went unannounced ("the
bring-up's No-Op command: Silent"), and no `DMA FAULT` line followed (PR
#921's evidence, `g3` logs).

So that test holds only that the message reached nobody; the AMD bench's
row `amdvi_usbd_drives_its_own_entry` holds the record, and that the vector
reached no CPU by the stop's census.

Owner: the orchestrator's AMD laptop track
(`issues/an-amd-vi-machine-isolates-no-device.md`).

**Exit:** under the QEMU `build/ci/qemu-version` names, that rewrite is
recorded against the claim, and the test asserts the record.
