---
status: open
kind: defect
opened: 2026-09-29
---

# A machine without an IOMMU refuses every claim; signed in-image drivers should claim there

Owner decision, 2026-10-17: distro hardware drivers may operate without
isolation when the hardware genuinely lacks it. This is one distro-wide rule,
not a per-driver exception list or a machine-specific image. Use available
isolation even for trusted distro drivers. Unknown isolation, unsupported
isolation hardware and failed setup remain errors; they do not enable a bypass.

External DMA-capable devices require isolation regardless of which driver
controls them. A trusted driver does not make external hardware trustworthy.
Determine the actual hardware boundary, including inseparable groups; do not
claim isolation between devices the hardware cannot separate. Ordinary USB
peripherals are not independent host-memory DMA requesters: distinguish the
host controller's DMA authority from the untrusted protocol data it handles.

Eligibility is authenticated distro hardware-driver identity and a legitimate
device grant, not merely being an executable in the image, a pathname, or a
signature on an independently installed package. The verified image covers
its drivers; separate driver signatures are not required. Establish how the
trusted launch path conveys that identity and authority to the kernel. Other
powers, including reset lines and firmware register access, remain explicit
grants and are not implied by this rule.

Use the same driver and DMA API in both modes. The DMA layer supplies translated
addresses when isolated and platform-correct bus addresses otherwise. CPU
process isolation remains, but an unisolated DMA driver/device belongs to the
trusted base and can potentially compromise the whole machine. Report this
reduced protection accurately. This policy does not itself implement a new
board port or authorize bypassing existing isolation to accelerate one.

The no-isolation claim path and its authenticated authority are not implemented.

Today `pcidev`'s `bring_up` refuses every claim on such a machine first with
`Refusal::NotRemapped`, whoever asks: no unit remaps its interrupts, and a
claimed function is armed only through `iommu::claim_msi`, which takes a
`Remapping` such a machine never mints. Behind that, `Refusal::Untranslated`:
a slot's space is an `iommu::OwnSpace`, which has no untranslated form, so
there is no untranslated device address for a grant to answer with. The distro
driver's claim is the path where `NotRemapped` and `Untranslated` both give way under
the ruling.

On such a machine whose image is on its NVMe disk, which
`Profile::HeadlessNoIommu` is, the refusal takes every volume but ROOT with
it: `diskserver` is refused the controller (`diskserver: NOT SERVING —
pci:1b36:0010 is on this machine and the kernel refused this service its
claim`), so the three file servers say `Data serving … — absent`, `Log serving
/log — absent` and `Boot serving /boot — absent`, and `logkeeper` says `no
/log on this machine - this boot's kernel log is on the console only`. Read off the no-unit arm of `iommu_virtio_platform`, which is
green with it: the arm asserts the refusal and reads no volume. **The kernel
drives no USB either** since usbd took the xHCI (the small-kernel track's step
10), so on such a machine usbd is refused its controller too, a USB stick's
volumes are absent like the NVMe disk's, and no USB keyboard or pointer
reaches the owner of input; the arm asserts that refusal as well. A machine
whose unit remaps no message, or filters no SMI, refuses the same claims for
those reasons (QEMU's AMD unit is both, by profile). On a bench that is the
stick too, so such a boot leaves no `/log` for a metal judge to read: the
metal rows that staged it (`iommu-no-remap`) are deleted, and QEMU's
`usbd_refused_without_amdvi_remapping` and `amdvi_firmware_left` hold the
refusal.

Exit, in both arms of `iommu_virtio_platform` (`tests/common/iommu.rs`):

- On `Profile::HeadlessNoIommu` an internal device claim by an authenticated
  distro hardware driver is handed over, and an ineligible claim is refused by
  a `Refusal` variant of its own, named on the `pcidev: … NOT HANDED OVER —` line
  `faults::refused_claim` reads, so a refusal for any other reason (no
  MSI-X, no BAR window) fails the arm. Deleting the identity check so that
  every claim goes through reds it. An ordinary test executable must remain
  ineligible even if shipped in the test image; it is not a distro hardware
  driver. Cover separately installed drivers in the authority policy tests.
- Report the lack of DMA isolation on `Profile::HeadlessNoIommu`, without
  implying that CPU process isolation is absent. The isolated profile must
  retain isolation and not report the unisolated mode.
- Host policy tests cover external devices, unknown topology, setup failures
  and driver eligibility: none acquires an unintended bypass. Relevant device
  tests verify that external DMA remains blocked without established isolation.
  Existing usable isolation must still be used for distro drivers.

The refusal is named in `pcidev`'s `Refusal`, not as a new `ClaimError`:
`ClaimError` splits only where a caller acts differently, and its one caller,
init's `refused`, answers every `Unusable` alike, by pointing at the
`pcidev:` line that names why.
