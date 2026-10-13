---
status: open
kind: defect
opened: 2026-10-12
---

# A virtio function that offers no `ACCESS_PLATFORM` is handed over as translated

Virtio 1.2 §6, `VIRTIO_F_ACCESS_PLATFORM`: "If this feature bit is set to 0,
then the device has same access to memory addresses supplied to it as the
driver has. In particular, the device will always use physical addresses
matching addresses used by the driver ... When clear, this overrides any
platform-specific description of whether device access is limited or
translated in any way, e.g. whether an IOMMU may be present." And §6.1: "If
VIRTIO_F_ACCESS_PLATFORM is not offered, then a driver MUST pass only physical
addresses to the device."

`pcidev` hands a process a function once the unit the firmware's tables put
it behind has given it a domain (`kernel/src/pcidev/mod.rs`), and never asks
whether a virtio function offers the bit; the holder's driver accepts it where
offered and goes on without it (`Offer::accept`, `lib/virtio/src/pci.rs`). A
function that does not offer it is not behind the unit at all: QEMU keeps it
on the machine's own address space whatever the `DMAR` says
(`tests/common/iommu.rs`). So an honest
holder's descriptors, which carry domain addresses above all memory
(`kernel/src/iommu/window.rs`), reach nothing, and a hostile holder's, which
carry physical addresses, reach all of memory: the claim's isolation is void
for exactly the functions it is promised for.

The launcher and the harness hide it. Each passes `iommu_platform=on` to every
virtio function wherever a unit exists (`build/qemu.rs`), and
`iommu_virtio_platform` reds a profile that does not; QEMU's own default is
off. A transitional virtio function cannot offer the bit in QEMU at all, and
VirtualBox's virtio core offers it nowhere
(`issues/the-virtio-drivers-match-only-a-non-transitional-device-id.md`).

The check is not the holder's, for the holder is who the domain protects the
machine from, nor the kernel's, which the owner's target keeps free of virtio
(`issues/every-driver-is-still-in-the-kernel.md`; `kernel/src/pcidev` and
`kernel/src/iommu` name none today). By the orchestrator's ruling (#896,
issuecomment-6131213771, ruling 1) it is the supervisor's, which mints the
claim and is trusted: it reads the function's offered features before it moves
the claim in, and on a machine whose functions are translated refuses a virtio
function that does not offer the bit. How it reads the word is the stage's
design.

**Owner**: whoever holds `issues/toyos-runs-in-any-open-source-hypervisor.md`.

**Exit**: a guest boot under the harness's VT-d with netstack's virtio-net
created without `iommu_platform=on` reads the supervisor's line refusing
netstack that function for its missing `VIRTIO_F_ACCESS_PLATFORM`, and
`netstack: no NIC on this machine`; the same boot with the check deleted
endows netstack the function; and `git grep -i virtio kernel/src/pcidev
kernel/src/iommu` still finds nothing.
