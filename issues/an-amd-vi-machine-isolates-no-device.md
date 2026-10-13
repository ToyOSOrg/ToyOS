---
status: assigned
kind: track
opened: 2026-10-10
---

# An AMD-Vi machine isolates no device

Held by the orchestrator's AMD laptop track: its stage 5, amdvi-translate, builds
translation, and its stage 6, amdvi-intremap, interrupt remapping.

On a machine whose IVRS describes AMD-Vi units, `kernel/src/arch/x86_64/amdvi/`
translates (stage 5): every requester id's entry blocks unless it names a
domain, enumerated functions sit on an identity domain until a kernel driver
moves one to its own, and the event log feeds `iommu::fault`. No interrupt
is remapped: every entry's interrupt half is invalid, so every
message-signalled interrupt is delivered as written, and a function a
process would drive is refused its claim (`iommu::remapping` is
`NotRemapped`), so no userland driver runs there.

What remains is interrupt remapping (stage 6): a remapping table per
requester indexed by vector, the I/O APICs' requester ids from the IVRS's
special entries, `claim_msi`, a claim clearing its entry's pass bits and
`SysMgt`, and a claim of a function an IVMD names refused.

Constraints a reader would otherwise pay to re-derive:

- QEMU 11.1.1's `amd-iommu` translates only with `dma-remap=on`; it takes a
  device table entry of `Mode` 0 as pass-through whatever `IR` and `IW` say,
  where the specification blocks it, so the blocked default is unobservable
  there; and its `IO_PAGE_FAULT` record carries no address
  (`issues/qemus-amd-iommu-page-fault-record-carries-no-address.md`).
- The AMD bench is in the metal suite; the orchestrator runs every boot.

**Exit:** a guest boot on `Profile::MetalAmdVi` and a run on the AMD bench
in which a kernel driver's DMA goes through a domain of its own, a DMA
outside it is recorded as a fault, and a claimed function's message is
delivered through its own remapping entry.
