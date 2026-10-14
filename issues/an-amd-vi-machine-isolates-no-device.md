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

What remains is a claim on the AMD bench: both its xHCI controllers keep
their MSI-X table in BAR 0, their only BAR, so `pcidev` refuses to hand one
over (`issues/a-controller-whose-msix-table-is-in-bar-0-cannot-be-driven-from-userland.md`),
and `xhci-leave=` names a controller by an id the two share.

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

**Exit:** a guest boot on `Profile::MetalAmdVi` and a run on the AMD bench
in which a kernel driver's DMA goes through a domain of its own, a DMA
outside it is recorded as a fault, and a claimed function's message is
delivered through its own remapping entry.
