---
status: open
kind: track
opened: 2026-08-02
---

# The IOMMU refuses nothing yet

Built: discovery, translation, interrupt remapping with every source in the
remappable format and every entry source-id-verified, and a domain per driver
— xHCI, NVMe, virtio-net, virtio-console, virtio-gpu, virtio-sound and the HDA
controller each reach their own pools and nothing else, each has an arm that
aims it at another driver's pool and reads the fault, and
`iommu_domain_isolation` walks the tables the unit reads on the three machines
that between them carry all seven and requires the page sets to be pairwise
disjoint. The fault handler clears Bus Master Enable, latches the first record
and counts per unit and per function before its terminal halt; the reschedule
handoff lands with the process-kill arm, and teardown's slow half never runs
from the deferred zero-handle queue.

**What remains is the refusal**, and it is sequenced after the first userspace
driver has moved (`issues/kernel/every-driver-is-still-in-the-kernel.md`):
before that it costs every machine with no vIOMMU — the default for QEMU,
VirtualBox, VMware, Parallels and essentially every cloud instance — and
protects nothing that has moved. Its two rules are ruled: the
extended-interrupt-mode rule is stated in terms of the x2APIC ids actually in
use — a machine whose ids all fit eight bits needs no EIM, which is what
interrupt remapping already does; and the isolation-scope rule refuses a
non-singleton scope only for a function behind a PCIe switch, never for a
root-complex-integrated function, because the latter has no peer to be
isolated from. The refusal lands with driver eviction, as the thing that
protects a moved driver.

What the harness cannot measure:

- **Invalidation.** QEMU fills its IOTLB only on the success path, so a
  missed post-map invalidation cannot show, and a missed post-unmap one would
  need a device re-aimed at a page already reused. Measured: with every
  invalidation removed, every IOMMU gate stayed green.
- **Compatibility-format blocking and the fault event's exemption**, recorded in
  `issues/kernel/qemu-passes-compatibility-format-interrupts.md` as T14
  questions.
- **Isolation scopes and reserved regions.** QEMU's topology is flat and
  publishes no RMRR; the T14 gives the first real answer.
- **Cost.** The 2× bar is answerable only on hardware, in a same-session A/B;
  so are cache-snooping walks, real access-control enforcement, and mid-DMA
  function reset on a real device.
