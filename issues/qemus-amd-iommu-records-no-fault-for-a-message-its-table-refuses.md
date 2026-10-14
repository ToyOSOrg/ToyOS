---
status: open
kind: tooling
opened: 2026-10-14
---

# QEMU's `amd-iommu` records no fault for a message its table refuses

AMD 48882 rev 3.11 §2.2.5.2, step 7: a message whose remapping entry has
`RemapEn` clear is target-aborted and reported as an `IO_PAGE_FAULT` with `I`
set (Table 57). Under QEMU 11.1.1's `amd-iommu` (`intremap=on,xtsup=on`,
128-bit entries), a claim's holder rewrote its MSI-X entry from the claim's
vector 0x42 to 0x43; usbd's No-Op went unannounced ("the bring-up's No-Op
command: Silent") and no event record was written (PR #921's evidence, `g3`
logs). Whether QEMU dropped the message or delivered it unremapped to 0x43,
which no claim held, was not measured.

QEMU's unit has no SMI filter (`SmiFSup` 0), so under the ruling on #921 it
hands no table's BAR over and the arm no longer runs there; the AMD bench's
`amdvi_usbd_drives_its_own_entry` holds the record and the census.

Owner: the orchestrator's AMD laptop track
(`issues/an-amd-vi-machine-isolates-no-device.md`).

**Exit:** under the QEMU `build/ci/qemu-version` names, a message on an
absent remapping entry is recorded against its requester, and a guest test
asserts the record.
