---
status: assigned
kind: defect
opened: 2026-09-26
---

# A controller whose MSI-X table is in BAR 0 cannot be driven from userland

Held by the orchestrator, with P1 of `issues/there-is-no-wifi.md`. Since
PR #921 (AMD-Vi stage 6), under the orchestrator's rulings there, a claim exists only where every message its function sends, whatever its
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

On the AMD bench that claim waits on P1 of `issues/there-is-no-wifi.md`
(a BAR stays where firmware put it, in 4 KiB pages): `03:00.4`'s only BAR,
which holds its table, sits behind a port whose window bus 3 fills, so it
cannot move to a 2 MiB slot of its own, and its claim is refused by name.

**Depends on:** P1 of `issues/there-is-no-wifi.md`.

**Exit:** P1's AMD bench reading of `03:00.4`, claimed with its BAR in place
and driven through its own remapping entry, its holder's foreign vector
refused and recorded; and a machine that does not remap still refusing the
claim (`amdvi_claim_refused_without_remapping`).
