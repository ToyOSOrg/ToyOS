---
status: open
kind: track
opened: 2026-08-02
---

# The IOMMU refuses nothing yet

**What remains is the refusal.** Its two rules are ruled: the
extended-interrupt-mode rule is stated in terms of the x2APIC ids actually in
use — a machine whose ids all fit eight bits needs no EIM, which is what
interrupt remapping already does; and the isolation-scope rule refuses a
non-singleton scope only for a function behind a PCIe switch, never for a
root-complex-integrated function, because the latter has no peer to be
isolated from.

What the harness cannot measure:

- **Invalidation.** QEMU fills its IOTLB only on the success path, so a
  missed post-map invalidation cannot show, and a missed post-unmap one would
  need a device re-aimed at a page already reused. Measured: with every
  invalidation removed, all five of the then five IOMMU gates stayed green.
- **Compatibility-format blocking and the fault event's exemption**, recorded in
  `issues/kernel/qemu-passes-compatibility-format-interrupts.md` as T14
  questions.
- **Isolation scopes and reserved regions.** QEMU's topology is flat and
  publishes no RMRR; the T14 gives the first real answer.
- **Cost.** The 2× bar is answerable only on hardware, in a same-session A/B;
  so are cache-snooping walks, real access-control enforcement, and mid-DMA
  function reset on a real device.

**Ruled** (owner, 2026-10-04), on the threat model, **"Refuse external,
record reflash"**: "External-port devices can't be claimed by userland
drivers for now; reflashing is a recorded weakness with an exit (firmware
verification) to design later." The weakness is
`issues/kernel/a-driver-that-reflashes-its-device-escapes-its-isolation.md`.
On the T14 the external ports are the Thunderbolt root ports `00:07.0` and
`00:07.2`, under units 1 and 2 (the orchestrator's reading of that machine's
`iommu:` lines).

**Ruled** (owner, 2026-10-04), on stage 2, the default-deny root and the
hand-over of a unit firmware left translating:

- **"Display and USB only"**: "Allowed only for display and USB controllers
  whose reserved region belongs to them alone; that memory is mapped into the
  driver's own isolated space. Everything else with reserved memory is
  refused." Reserved memory is a DMAR RMRR; on the T14 the one RMRR,
  `0x9c000000..0xa07fffff`, names the iGPU `00:02.0` alone.
- **"Accept and file"**: "Allow the one-moment gap on those machines, log it,
  and file it as a known weakness with a fix (older protection registers) as
  its exit." The gap, as put to him: a unit firmware left translating in
  scalable or abort-DMA mode, with `CAP.ESRTPS` clear, may not have its root
  table switched under translation (VT-d Rev. 4.1 §6.6), so translation goes
  off for that one switch. The older protection registers are `PMEN`'s
  protected memory regions (§11.4.8.1). The T14's units report `ESRTPS`,
  `SMTS` and `ADMS` clear, so it is not one of those machines.
- **"Allow it"**, on the T14 row in which a test program claims the iGPU and
  its reserved region is mapped into its domain: "One test boot with a blank
  panel, only in that row; normal boots are unaffected."
