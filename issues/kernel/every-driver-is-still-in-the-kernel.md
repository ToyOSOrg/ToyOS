---
status: open
kind: track
opened: 2026-08-02
---

# Every driver is still in the kernel, and moving one before the IOMMU is finished costs security

The owner's target is that `virtio` does not appear anywhere in the kernel. The
strongest check on it is not the driver files but the ABI: no syscall name may
carry a NIC, GPU or audio device operation. `rg '^pub const SYS_(NIC|GPU|AUDIO)_'
toyos-abi/src/syscall.rs` is the check — the three `SYS_NIC_*` names went with
the virtio NIC driver, which is `userland/netd/src/virtio_net.rs` now, and the
`SYS_GPU_*` names are what is left.

**Ordering ruling, and it is not negotiable: the IOMMU lands first and
completely, interrupt remapping included, and no driver leaves the kernel before
it.** Moving a driver out without translation *costs* security — a descriptor
holding a physical address is an arbitrary read/write primitive over all of
memory. `kernel/src/pcidev/mod.rs` is where that ruling is enforced for a
function a process drives: a claim on one this machine cannot give an address
space of its own is refused by name, so there is no machine on which a driver
outside the kernel gets an untranslated address.
`issues/kernel/the-iommu-refuses-nothing-yet.md` still holds the other half —
every driver *inside* the kernel holds a domain of its own and the refusal there
is not built.

What is left of the staged work:

1. **The kernel's audio registry is a concrete match on a device type.** The
   file this was scoped against has since been deleted, so this needs re-scoping
   before it can start; the GPU trait is the model to copy.
2. Done: **BAR sizing and re-assignment onto 2 MiB boundaries** is
   `pcidev::place_bar`, with the overlap refusal kept as the assertion that it
   worked rather than as the mechanism.
3. Done: **the capability itself** is `DeviceType::PciFunction` plus
   `SYS_DEVICE_BAR_MAP` and `SYS_DEVICE_DMA_ALLOC`, with config space readable
   and unwritable and the interrupt delivered as a record on the claim.

Two constraints that were not obvious before the code was read:

- **USB HID cannot move to userspace without moving the boot block device or
  splitting the controller.** It shares the controller, the event ring and the
  lock with the boot disk.
- The exception criterion is "a driver stays in the kernel only if the kernel
  needs it while userspace is dead". A widening to "if a *service the kernel
  itself provides* needs it while userspace is dead" — which keeps NVMe and
  changes nothing else — is waiting on the owner.
