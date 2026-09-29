---
status: open
kind: defect
opened: 2026-09-29
---

# A machine without an IOMMU refuses every claim; signed in-image drivers should claim there

The owner ruled 2026-09-29 (recorded in
`issues/hardware/the-t14-answers-only-through-a-usb-stick.md` and
`issues/kernel/every-driver-is-still-in-the-kernel.md`, superseding the
2026-09-07 ruling there) that a machine with no IOMMU is supported hardware —
the Raspberry Pi is a wanted target
(`issues/kernel/toyos-runs-on-the-raspberry-pi.md`) — and that on such a
machine only a driver that is signed and shipped in the ToyOS image may claim
a device, never a user-installed one; the kernel's DMA layer hands that
driver the physical address directly rather than one mapped through a
domain. That distinction is not built.

Today `kernel/src/iommu/mod.rs`'s `DeviceSpace::own()` — which
`kernel/src/pcidev/mod.rs`'s `slot_space` calls for every userland claim —
answers `Err` whenever the machine has no unit, with no way to say who is
asking; `bring_up` turns that into `Refusal::Untranslated` and `claim` refuses
unconditionally. `DeviceSpace::create()`, a few lines below `own()`, already
has the shape the ruling wants for a driver still inside the kernel — `Own`
where a domain exists, `Untranslated` where it does not — `own()` needs the
same latitude for a userland claim, gated on the claimant being a signed,
in-image driver rather than on nothing.

Exit: a test that claims a device on a machine with no IOMMU unit
(`iommu_virtio_platform`'s no-unit arm, or an equivalent) passes for a driver
the image ships and signs, and still refuses with `ClaimError::Unusable` for
one that is not; a mutation that deletes the identity check and lets every
claim through must fail that test. The claim succeeding is not the isolation
guarantee: an isolation test run against that same machine reports "no
isolation on this machine", never a pass, and that report is part of this
exit, not a separate one.
