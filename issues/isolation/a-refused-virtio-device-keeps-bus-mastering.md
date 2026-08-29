---
status: open
kind: defect
opened: 2026-08-29
---

# A virtio device the parser refused keeps bus mastering

`VirtioDevice::init` enables bus mastering before it parses the capability
chain (`kernel/src/drivers/virtio.rs:761` then `:763`), so a device whose
chain is refused — `Err(MissingCap::…)`, the driver logs and returns — is left
with the COMMAND register's bus-master bit set and no driver, no queues and no
reset behind it. A function the kernel just declined to trust can still master
the bus, which is an isolation hole of exactly the class the IOMMU track
carries: the refusal answers the kernel's side and leaves the device's side
armed.

Pre-existing in shape: before the refusal existed the same order held and a
missing capability was a boot panic, so no refused-and-running state was
reachable; the refusal (adversarial review of PR #339, which also found this)
made it reachable without making the order right. The fix direction is either
enabling bus mastering only after `parse` succeeds — nothing between the two
needs DMA — or disabling it on the refusal path, and either wants a case in
the `pci_capability_walk` selftest asserting the COMMAND bit's state after a
refused parse.
