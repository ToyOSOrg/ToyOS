---
status: assigned
kind: track
opened: 2026-10-10
---

# An AMD-Vi machine isolates no device

Held by the orchestrator's AMD laptop track: its stage 5, amdvi-translate, builds
translation, and its stage 6, amdvi-intremap, interrupt remapping.

On a machine whose IVRS describes AMD-Vi units, `kernel/src/arch/x86_64/amdvi/`
translates (stage 5) and remaps (stage 6): every requester id's entry blocks
unless it names a domain, and where every programmed unit has `GASup` (and
`XTSup` on an x2APIC machine) and names every I/O APIC, every requester's
fixed and arbitrated messages go through a table of its own indexed by
vector, a claim's entry passing nothing firmware declared.

What remains is the AMD bench's reading of a claim (PR #921 stages it): a
claimed `03:00.4` driven through its own entry, its MSI-X table's BAR handed
over because every unit filters SMIs, its holder's foreign vector refused
and recorded, and a machine that remaps nothing refusing the claim.

Constraints a reader would otherwise pay to re-derive:

- QEMU 11.1.1's `amd-iommu` translates a function only with `dma-remap=on`,
  and only once an `INVALIDATE_DEVTAB_ENTRY` names it
  (`issues/qemus-invalidate-iommu-all-leaves-every-function-untranslated.md`); it takes a
  device table entry of `Mode` 0 as pass-through whatever `IR` and `IW` say,
  where the specification blocks it, so the blocked default is unobservable
  there; and its `IO_PAGE_FAULT` record carries no address
  (`issues/qemus-amd-iommu-page-fault-record-carries-no-address.md`).
- With `intremap=on` its IVRS names the I/O APIC (special entry, variety 1,
  handle 0, as `00:14.0`), and with `xtsup=on` its EFR sets `XTSup`; this
  suite's guests run in x2APIC, so a remapping guest needs both.
- The AMD bench is in the metal suite; the orchestrator runs every boot.

**Exit:** on the AMD bench, `amdvi_usbd_drives_its_own_entry` and
`amdvi_claim_refused_without_remapping` pass beside
`amdvi_translates_the_stick_controller` and
`amdvi_remaps_the_stick_controller`: a kernel driver's DMA through a domain
of its own, a DMA outside a claim's domain recorded, a claimed function's
message delivered through its own remapping entry and a foreign one refused
and recorded.
