---
status: open
kind: defect
opened: 2026-10-04
---

# A driver that reflashes its device escapes its isolation

A userland driver holds its device's registers, and through them many
devices take new firmware (NVMe's Firmware Image Download and Commit, for
one). Nothing in ToyOS refuses that or checks what a device runs. The IOMMU
confines a device by the requester ID its requests carry, so it confines a
reflashed one only as far as that ID is the device's own: ACS Source
Validation at a port refuses another bus's ID from below it, but nothing
stands between a root-complex-integrated endpoint, or one function of a
multi-function device, and an ID beside it. And the firmware outlives the
driver: the next holder, and the next boot's firmware before any IOMMU is
on, drive what the last holder wrote. That reach is the IOMMU design's
reading, not a measurement.

Owner decision, 2026-10-17: external DMA-capable devices still require
isolation, even when operated by distro drivers. The no-isolation policy in
`issues/a-machine-without-an-iommu-refuses-every-claim.md` does not waive that
boundary or solve firmware persistence. Reflashing remains the recorded
weakness whose firmware-verification exit needs design.

Owner: the orchestrator, under `issues/the-iommu-refuses-nothing-yet.md`.

**Exit**: a design for verifying a device's firmware is put to the owner
and ruled, and built; a device whose firmware fails that verification is
refused its claim by name, and a test or T14 row reads the refusal and its
negative control.
