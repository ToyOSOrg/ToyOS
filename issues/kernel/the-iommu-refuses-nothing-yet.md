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
