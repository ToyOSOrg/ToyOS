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
(`hw/virtio/virtio-bus.c`, which `tests/common/iommu.rs` cites). So an honest
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

The check cannot be the holder's: the holder is who the domain protects the
machine from.

**Owner**: whoever holds `issues/toyos-runs-in-any-open-source-hypervisor.md`.

**Exit**: a claim on a virtio function that does not offer
`VIRTIO_F_ACCESS_PLATFORM` behind a unit is refused by a `Refusal` of its own;
a guest boot under the harness's VT-d with netstack's virtio-net created
without `iommu_platform=on` reads that refusal on its `pcidev: … NOT HANDED
OVER —` line, and is handed over with the check deleted.
