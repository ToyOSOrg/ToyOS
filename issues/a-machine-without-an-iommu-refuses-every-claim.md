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
unit and a domain address where there is. A claim's space is an
`iommu::OwnSpace` (`kernel/src/iommu/mod.rs`), which has no untranslated form
until the ruled path adds one. Such a machine states plainly that it has no
isolation — the full isolation guarantee needs an IOMMU, and there a bad
signed driver can still crash or corrupt the system.

**Not yet designed, and owed:** what "signed" is (what is signed, by whom, and
where it is checked) and so what the "identity check" compares. No signature
or image-identity mechanism exists in the kernel today. The exit below cannot
be written as a test until it is.

Today `pcidev`'s `bring_up` refuses every claim on such a machine first with
`Refusal::NotRemapped`, whoever asks: no unit remaps its interrupts, and a
claimed function is armed only through `iommu::claim_msi`, which takes a
`Remapping` such a machine never mints. Behind that, `Refusal::Untranslated`:
a slot's space is an `iommu::OwnSpace`, which has no untranslated form, so
there is no physical address for a grant to answer with. The signed driver's
claim is the path where `NotRemapped` and `Untranslated` both give way under
the ruling.

On such a machine whose image is on its NVMe disk, which
`Profile::HeadlessNoIommu` is, the refusal takes every volume but ROOT with
it: `diskserver` is refused the controller (`diskserver: NOT SERVING —
pci:1b36:0010 is on this machine and the kernel refused this service its
claim`), so the three file servers say `Data serving … — absent`, `Log serving
/log — absent` and `Boot serving /boot — absent`, and `logkeeper` says `no
/log on this machine - this boot's kernel log is on the console only`. Read off the no-unit arm of `iommu_virtio_platform`, which is
green with it: the arm asserts the refusal and reads no volume. Where the
image is on a USB stick the kernel drives, LOG and BOOT are served through
kernel partition claims, which need no unit, and DATA alone is absent; that
is read off the claim path and not run.

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
