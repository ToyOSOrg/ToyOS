---
status: assigned
kind: defect
opened: 2026-09-26
---

# A controller whose MSI-X table is in BAR 0 cannot be driven from userland

Held by PR #921 (AMD-Vi stage 6), under the orchestrator's rulings there:
a claim exists only where every message its function sends, whatever its
holder writes into its MSI-X table or aims a DMA at, reaches only its own
interrupt, and there `pcidev` hands over the table's BAR like any other.
VT-d holds that by source-id verification and its block on
compatibility-format messages, the ITS by the device id the bridge stamps,
and AMD-Vi by the requester's own table together with every unit's SMI
filter, since a write to the interrupt range is an interrupt (48882
§2.1.4.2) and an SMI no remapped message (Table 10). Where an AMD-Vi unit
filters no SMI, every claim is refused by that reason.

QEMU's AMD unit has no SMI filter, so QEMU drives no claim behind it:
`usbd_refused_without_smi_filter` reads the refusal, and only the AMD bench
reads a claim through an AMD-Vi entry.

**Exit:** on the AMD bench, `03:00.4` (MSI-X table in BAR 0, its only BAR)
is claimed and driven by usbd through its own remapping entry with every
unit filtering SMIs, and its holder's rewrite of its entry to a vector not
its own is refused, recorded against the claim, and reaches no CPU
(`amdvi_usbd_drives_its_own_entry`); and a machine that does not remap still
refuses the claim (`amdvi_claim_refused_without_remapping`).
