---
status: open
kind: track
opened: 2026-10-10
---

# An AMD-Vi machine isolates no device

On a machine whose IVRS describes AMD-Vi units, `kernel/src/arch/x86_64/amdvi/`
switches every unit off, so every device's DMA reaches all of memory and every
message-signalled interrupt is delivered as written. `DeviceSpace::create`
answers `Untranslated`, and a function a process would drive is refused its
claim (`iommu::remapping` is `NotRemapped`), so no userland driver runs there.

What remains is translation, then interrupt remapping:

- **Translation.** A device table blocking by default, every one of the 65,536
  entries `V=1, TV=1` with `IR=IW=0` — an entry left `V=0` passes its
  requester's DMA untranslated — identity domains for kernel drivers, a
  command buffer whose `COMPLETION_WAIT` is waited on under a `Tripwire`, and
  the event log feeding `iommu::fault`.
- **Interrupt remapping.** Interrupt remapping tables, the I/O APIC's and the
  HPET's requester ids from the IVRS's special entries, and `claim_msi`.

Constraints a reader would otherwise pay to re-derive:

- QEMU 11.1.1's `amd-iommu` translates a function only with `dma-remap=on`,
  and only once an `INVALIDATE_DEVTAB_ENTRY` names it; it takes a device table
  entry of `Mode` 0 as pass-through whatever `IR` and `IW` say, where the
  specification blocks it (`hw/i386/amd_iommu.c` at v11.1.1).
- No AMD machine is in reach of the metal suite.

**Exit:** a guest boot on `Profile::MetalAmdVi` in which a kernel driver's
DMA goes through a domain of its own, a DMA outside it is recorded as a
fault, and a claimed function's message is delivered through its own
remapping entry.
