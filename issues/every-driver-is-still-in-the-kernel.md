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
outside the kernel gets an untranslated address. The ruling
for signed in-image drivers on a machine with no IOMMU is
`issues/a-machine-without-an-iommu-refuses-every-claim.md`'s.
`issues/the-iommu-refuses-nothing-yet.md` still holds the other half —
every driver *inside* the kernel holds a domain of its own and the refusal there
is not built.

What is left of the staged work:

1. **HDA and virtio-gpu, re-scoped.** `drivers/hda.rs` brings its device up
   and gates soundserver's register access; `drivers/virtio_gpu.rs` is the only
   `Gpu` whose `SYS_GPU_*` calls do anything, since GOP's are all no-ops. Each
   leaves when its userland holder claims the function as `pci`, as netstack
   and soundserver's virtio-sound driver do, retiring the `hda-audio` class,
   its arms of `SYS_DEVICE_REG_READ`/`WRITE`, and `SYS_GPU_*`, which is an ABI
   change. GOP stays: it is memory the loader hands over, and the panic console
   paints it. HDA leaving feeds soundserver's DLL from a claim's record on
   every machine, so that diff lands
   `issues/soundservers-virtio-clock-can-take-a-period-one-notification-early.md`'s
   exit.
   A virtio holder stands on `toyos-virtio`, which walks the capability list
   too. Before a client ends its device on `UsedRefusal::Written` for a chain
   the device only reads, whose bound is 0, what QEMU's device reports as
   `len` on such a queue is measured: the NIC's transmit queue is the only
   one read so far.
   `iommu_virtio_platform`'s decline control, `declining_is_not_free`
   (`tests/common/iommu.rs`), declines through the kernel's
   `virtio-no-access-platform` actuator, which reaches only a virtio function
   the kernel drives; with the NIC and virtio-sound in processes that is the
   virtio-gpu `Profile::HeadlessVirtioGpu` adds for it, and virtio-gpu leaving
   leaves the control nothing to decline. Exit for the control, in the diff
   that moves virtio-gpu out: the decline is made where the features are
   negotiated, `toyos-virtio`'s `Offer::accept`, for a function a process
   drives, and the control reds when that function keeps `FEATURES_OK`;
   `Profile::HeadlessVirtioGpu` goes in the same diff.
2. Done: **BAR sizing and re-assignment onto 2 MiB boundaries** is
   `pcidev::place_bar`, with the overlap refusal kept as the assertion that it
   worked rather than as the mechanism.
3. Done: **the capability itself** is `DeviceType::PciFunction` plus
   `SYS_DEVICE_BAR_MAP` and `SYS_DEVICE_DMA_ALLOC`, with config space readable
   and unwritable and the interrupt delivered as a record on the claim.
4. **The i8042 (PS/2)**: staged as stage 7 of
   `issues/the-kernel-is-small-interrupts-post-and-threads-wait.md`.

- **USB HID cannot move to userspace without moving the boot block device or
  splitting the controller.** It shares the controller, the event ring and the
  lock with the boot disk.
