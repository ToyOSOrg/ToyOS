---
status: open
kind: track
opened: 2026-10-12
---

# ToyOS runs in any open-source hypervisor

The owner's aim, as the orchestrator briefed this audit: ToyOS runs in any
popular open-source virtual machine — QEMU under KVM, HVF or TCG, Cloud
Hypervisor, VirtualBox, bhyve and crosvm — with QEMU still the launcher and the
test runner; under virtualization it rests on UEFI, ACPI, PCIe and virtio 1.2
and the real devices it already drives (NVMe, xHCI and USB HID, the Intel NICs,
the i8042), and nothing it ships depends on a QEMU-only device or behaviour.

Nothing in this tree records a boot of this image under any hypervisor but
QEMU. What this file and the issues it lists say of the others is read from
their documentation and source at their default branches, not measured; the
source is read clean-room, so the files and commits it was read from are
provenance in
https://github.com/ToyOSOrg/evidence/blob/0b27193281e0baee2d7a135f6c17934b5d6b456f/sources.md,
and nothing here cites them. Cloud Hypervisor and crosvm run on Linux hosts
and bhyve on FreeBSD, none of them on the macOS development host.

## What the shipped OS takes from QEMU

The kernel, the loader and every program drive no fw_cfg, ramfb, pvpanic,
isa-debug-exit, debugcon or Bochs display, and test for no hypervisor
signature. What they do depend on, each filed:

- `issues/the-virtio-drivers-match-only-a-non-transitional-device-id.md`
- `issues/the-x86-clock-calibrates-against-an-hpet-or-the-boot-panics.md`
- `issues/a-hardware-reduced-x86-machine-has-no-power-off-and-no-power-button.md`
- `issues/no-claim-gets-an-address-space-under-virtio-iommu.md`
- `issues/the-ap-trampoline-is-written-at-0x8000-whatever-the-memory-map-says-is-there.md`

and, filed before this track:

- `issues/a-machine-without-an-iommu-refuses-every-claim.md` — bhyve; crosvm
  for every device it emulates; and VirtualBox, QEMU and Cloud Hypervisor by
  default (`issues/no-claim-gets-an-address-space-under-virtio-iommu.md`).
- `issues/pci-is-enumerated-on-segment-group-0-alone.md` — Cloud
  Hypervisor's `num_pci_segments`.

AMD-Vi isolation (`kernel/src/arch/x86_64/amdvi/`) applies only where a
target is asked for AMD-Vi, VirtualBox's `--iommu amd` and QEMU's
`amd-iommu`, and no target's default asks; QEMU's unit has no SMI filter, so
a claim is refused there.

Two of the launcher's QEMU options keep two of these off every guest test:
the `-non-transitional` virtio devices the transitional id, and
`iommu_platform=on` the missing `ACCESS_PLATFORM` (`build/qemu.rs`,
`tests/common/qemu.rs`). The rest of what is
QEMU's there — the xHCI models, the boot stick, `-vga std`, `ramfb` under
AArch64's edk2, `caching-mode=on`, `kernel-irqchip`, `iommu-testdev`, QMP — is
the harness's alone.

Adaptations that degrade nothing: the TCO table's q35 row (`lib/tco`, armed
only by the `watchdog` parameter and refused by name elsewhere), netstack's
82574L row for QEMU's `e1000e`, the hard-lockup bound's path for a machine
with no PMU, `kernel::cpu`'s reading of `CPUID.1:ECX[31]`, which Intel
defines as "Not Used: Intel processors always return 0. Allocated for use by
software emulation" (Intel SDM Vol. 1, 253665-093US, Table 21-9) and AMD as
the hypervisor's (APM Vol. 3 §E.3.2), and the loader's read of PI's DXE services (`loader/src/gcd.rs`),
which hands over the root bridges' windows alone where firmware has none.

## Each hypervisor, x86-64

| | Firmware | Disk | Display | Network | Input | Platform | IOMMU |
|---|---|---|---|---|---|---|---|
| QEMU q35 | edk2 | NVMe `1b36:0010`, virtio-blk, AHCI, USB | std-vga GOP, virtio-gpu | virtio-net (transitional by default), e1000e | i8042, USB HID, virtio-input | HPET, full ACPI | VT-d, AMD-Vi, virtio-iommu |
| Cloud Hypervisor | `CLOUDHV.fd` (edk2) | virtio-blk | none | virtio-net | none | no HPET; hardware-reduced ACPI; an I/O APIC its MADT lists always, which its `docs/device_model.md` disables without the serial port | virtio-iommu, per device and off by default |
| crosvm | a `--bios` image; which UEFI boots under it was not read | virtio-blk, virtio-scsi | virtio-gpu | virtio-net | virtio-input | no HPET, no PM timer | virtio-iommu for a passed-through host device alone |
| VirtualBox 7 | EFI with `--firmware efi`; PCIe and MSI only with `--chipset ich9` | NVMe `80ee:4e56`, AHCI, USB, virtio-scsi | VMSVGA GOP | 8254x e1000s, virtio-net `1af4:1000` | i8042, USB HID | HPET off by default | VT-d or AMD-Vi with `--iommu`, off by default |
| bhyve | `BHYVE_UEFI.fd` (edk2) | NVMe `fb5d:0a0a`, AHCI, legacy virtio-blk | `fbuf` GOP | 82545EM, legacy virtio-net | i8042, xHCI tablet | HPET, full ACPI; HDA on INTx only | none |

First stop, on paper. VirtualBox's defaults, BIOS and PIIX3, boot nothing of
this image and enumerate no PCIe; with `--firmware efi` and `--chipset ich9`
it stops, like Cloud Hypervisor and crosvm, at the HPET `expect`, which on
Cloud Hypervisor no console carries unless its serial port is on. VirtualBox
with `--x86-hpet on` too, and bhyve: a desktop on a GOP panel with i8042 or
USB input, no disk, no network, and on bhyve no sound (below).

Once the dependencies close, each still lacks a driver this tree does not
have: virtio-blk on Cloud Hypervisor and crosvm, whose only disk it is;
virtio-input on crosvm, whose only input it is; and on bhyve, whose virtio is
legacy, a driver for its 82545EM, its only other NIC. VirtualBox lacks none:
its NVMe, GOP, i8042, xHCI and HDA are driven here, and its virtio-net once its
transitional id is.

Unsupported, and not owed: bhyve's HDA, which raises INTx alone. ToyOS arms
MSI-X or MSI only (`pcidev`'s `bring_up`)
and takes up no line-based interrupt (the orchestrator's ruling, #896,
issuecomment-6131213771, ruling 3), so bhyve has no sound.

## AArch64

Outside `issues/toyos-runs-on-arm64.md`'s ruled target, QEMU `virt` on the
development Mac, and read only:

- **Cloud Hypervisor**: `CLOUDHV_EFI.fd`, GICv3 with an ITS, the PL011 in the
  SPCR, virtio-iommu, no display.
- **VirtualBox 7**: UEFI always, virtio-scsi its only disk, its GIC's ITS off
  by default (`--arm-gic-its`).
- **bhyve**: U-Boot as firmware (`bhyve(8)`); whether it publishes an ACPI
  RSDP, without which the loader stops, was not read.
- **crosvm**: not read.

## Exit

The seven dependencies this file lists, the five filed before them and the
drivers each hypervisor still lacks closed; and the same image, built by
`cargo run -- --build-only`, booted by hand once under each of Cloud
Hypervisor, crosvm, VirtualBox and bhyve on x86-64 to `Boot: complete` with
DATA served and a DHCP lease, each boot's log on the evidence repository under
the hypervisor's version and command line.

**Blocked on**: the owner's rulings on where a virtio-iommu driver lives, on
both architectures, against his target of a kernel without virtio, and on
interrupt remapping under virtio-iommu on x86-64
(`issues/no-claim-gets-an-address-space-under-virtio-iommu.md`); the
signed-driver design `issues/a-machine-without-an-iommu-refuses-every-claim.md`
owes, and a host for each hypervisor.

**Owner**: the orchestrator; work starts on the owner's go.
