---
status: assigned
kind: defect
opened: 2026-09-26
---

# A controller whose MSI-X table is in BAR 0 cannot be driven from userland

Held by PR #921 (AMD-Vi stage 6), under the orchestrator's rulings there:
where the machine fences a message written into a claim's MSI-X table,
`pcidev` hands over the BAR holding it; where it does not, that BAR is kept
back, said by name. VT-d fences by source-id verification and its block on
compatibility-format messages, the ITS by the device id the bridge stamps,
and AMD-Vi by the requester's own table together with every unit's SMI
filter, since an SMI is no remapped message (48882 Table 10).

QEMU's AMD unit has no SMI filter, so `usbd_table_kept_without_smi_filter`
reads its spare's table BAR kept back and the claim refused
`NoMappableBar`.

**Exit:** on the AMD bench, `03:00.4` (MSI-X table in BAR 0, its only BAR)
is claimed and driven by usbd through its own remapping entry with every
unit filtering SMIs, and its holder's rewrite of its entry to a vector not
its own is refused, recorded against the claim, and reaches no CPU
(`amdvi_usbd_drives_its_own_entry`); and a machine that does not remap still
refuses the claim (`amdvi_claim_refused_without_remapping`).
