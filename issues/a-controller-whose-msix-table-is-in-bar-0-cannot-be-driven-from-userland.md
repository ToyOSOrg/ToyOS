---
status: assigned
kind: defect
opened: 2026-09-26
---

# A controller whose MSI-X table is in BAR 0 cannot be driven from userland

Held by PR #921 (AMD-Vi stage 6), under the orchestrator's ruling there:
on a machine where every unit remaps, `pcidev` hands over the BAR holding a
function's MSI-X table, because the unit's per-requester table is the guard
(a message the holder writes reaches only its own claim's interrupt, and
anything else is refused and recorded); where a unit does not remap, the
claim is refused anyway (`Refusal::NotRemapped`).

A claim exists only on a machine that remaps, so `kernel/src/pcidev/mod.rs`
no longer keeps the table's BAR back. QEMU's `qemu-xhci` with its table in
its register BAR is claimed and driven behind the AMD unit
(`usbd_drives_the_spare_amdvi`), and its holder's rewrite of its own entry
to a vector not its own reaches nobody; with the BAR withheld as before, the
claim is refused `NoMappableBar`.

**Exit:** on the AMD bench, `03:00.4` (MSI-X table in BAR 0, its only BAR)
is claimed and driven by usbd through its own remapping entry, and its
holder's rewrite of its entry to a vector not its own is refused and
recorded against the claim (`amdvi_usbd_drives_its_own_entry`); and a
machine that does not remap still refuses the claim
(`amdvi_claim_refused_without_remapping`).
