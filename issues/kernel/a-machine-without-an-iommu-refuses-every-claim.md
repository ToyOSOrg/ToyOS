---
status: open
kind: defect
opened: 2026-09-29
---

# A machine without an IOMMU refuses every claim; signed in-image drivers should claim there

Owner ruling, 2026-09-29: on a machine with no IOMMU unit a driver signed and
shipped in the ToyOS image may claim a device, and no other may.
It is the same driver code as on a machine with a unit, never a second
driver: the kernel's DMA layer hands it a physical address where there is no
unit and a domain address where there is (`DeviceSpace`,
`kernel/src/iommu/mod.rs`). Such a machine states plainly that it has no
isolation — the full isolation guarantee needs an IOMMU, and there a bad
signed driver can still crash or corrupt the system.

**Not yet designed, and owed:** what "signed" is (what is signed, by whom, and
where it is checked) and so what the "identity check" compares. No signature
or image-identity mechanism exists in the kernel today. The exit below cannot
be written as a test until it is.

Today `pcidev`'s `bring_up` refuses every claim on such a machine with
`Refusal::Untranslated`, whoever asks. A claim that goes on without a domain
cannot pass through `slot_space` as it stands: it calls `reserve`, grants
call `place`, and both panic on `DeviceSpace::Untranslated` — a kernel panic
a userland claim would reach.

Exit, in both arms of `iommu_virtio_platform` (`tests/common/iommu.rs`):

- On `Profile::HeadlessNoIommu` a claim by a driver the image ships and signs
  is handed over, and a claim by any other is refused by a `Refusal` variant
  of its own, named on the `pcidev: … NOT HANDED OVER —` line
  `faults::refused_claim` reads, so a refusal for any other reason (no
  MSI-X, no BAR window) fails the arm. Deleting the identity check so that
  every claim goes through reds it. The non-image claimant is a
  `tests/toyos-rust-tests` binary, to be added, that init hands the NIC
  function under `netcase`; netd is the in-image claimant.
- The kernel says "no isolation on this machine" at boot on
  `Profile::HeadlessNoIommu` and never on `Profile::Headless`. Printing it on
  every profile reds the `Headless` arm; printing it on none reds the other.

The refusal is named in `pcidev`'s `Refusal`, not as a new `ClaimError`:
`ClaimError` splits only where a caller acts differently, and its one caller,
init's `refused`, answers every `Unusable` alike, by pointing at the
`pcidev:` line that names why.

**`iommu_virtio_platform` is deleted**
(`issues/build/iommu-virtio-platform-reads-netds-line-off-a-boot-log-that-ends-before-it.md`):
the arms the exit names come back with it.
