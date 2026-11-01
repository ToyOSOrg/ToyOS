---
status: open
kind: track
opened: 2026-08-02
---

# Every driver is still in the kernel, and moving one before the IOMMU is finished costs security

The owner's target is that `virtio` does not appear anywhere in the kernel. The
strongest check on it is not the driver files but the ABI: no syscall name may
carry a NIC, GPU or audio device operation. `rg '^pub const SYS_(NIC|GPU|AUDIO|HDA)_'
sdk/abi/src/syscall.rs` is the check, and it finds none: the three `SYS_NIC_*`
names went with the virtio NIC driver, which is `system/virtio-net/src/nic.rs`
now, the four `SYS_GPU_*` names with virtio-gpu's, which is the
compositor's (`system/compositor/virtio-gpu`), and `SYS_HDA_BIND` with the
kernel's HDA stub. No virtio code is left in the kernel: its console's is
`system/virtio-console`'s, the virtio-serial `drives` row, and the kernel
drives no audio hardware.

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

1. Done for HDA: an `hda` process per function of `pci:04:03:*` (a
   `drives` row, `system/hda`) drives it whole — controller reset, CORB/RIRB,
   the stream and its descriptor list, the ring soundserver lends it, its
   interrupt as the claim's record — and offers soundserver its output
   (`issues/a-device-function-is-driven-from-its-arrival-to-its-end.md`'s
   stage 3). The `hda-audio` class,
   `SYS_HDA_BIND`, `SYS_DEVICE_REG_WRITE` and the kernel's HDA vector are
   gone; `SYS_DEVICE_REG_READ` reads a PCI claim's config space alone. Every
   machine's DLL is clocked by a claim's record, and a period read before its
   record's message landed waits for the next record's time (`Dll::update`'s
   `clocked`). GOP stays: it is memory the loader hands over, and the panic
   console paints it.
   A virtio holder stands on `toyos-virtio`, which walks the capability list
   too. Before a client ends its device on `UsedRefusal::Written` for a chain
   the device only reads, whose bound is 0, what QEMU's device reports as
   `len` on such a queue is measured: the NIC's transmit queue is the only
   one read so far.
   Done for virtio-gpu: the compositor drives it through a `pci:03:80:00/1af4:1050`
   claim. Its cursor queue's `len` is measured: QEMU writes 0 for a chain
   with a writable answer and for one without (PR #895's evidence, `pr-895`
   at `7d47ad2b`). The decline control `declining_is_not_free` and the
   `virtio-no-access-platform` actuator went with it, by the orchestrator's
   ruling on PR #895, rather than the decline moving into `Offer::accept`:
   with the kernel driving only its console the actuator reached nothing,
   and what ToyOS relies on is `toyos-virtio`'s, host-tested — the bit
   accepted wherever offered, and a device that drops `FEATURES_OK`
   refused. `Profile::HeadlessVirtioGpu` stays, the suite's one machine whose
   display a process drives.
2. Done: **BAR sizing, and each BAR handed over in place in its own 4 KiB
   pages**, is `pcidev::publish` and `pcidev::bars`.
3. Done: **the capability itself** is `DeviceType::PciFunction` plus
   `SYS_DEVICE_BAR_MAP` and `SYS_DEVICE_DMA_ALLOC`, with config space readable
   and unwritable and the interrupt delivered as a record on the claim.
4. **The i8042 (PS/2)**: staged as stage 7 of
   `issues/the-kernel-is-small-interrupts-post-and-threads-wait.md`.

- **USB HID moved with the boot disk**: usbd drives each xHCI controller
  whole, its sticks and its HID devices alike (the small-kernel track's step
  10).
- **Where each xHCI keeps its MSI-X table**, which decides whether usbd's
  claim arms MSI-X or MSI (`bring_up`, `kernel/src/pcidev/mod.rs`). QEMU
  11.1's `nec-usb-xhci` arms MSI-X and publishes one memory BAR, BAR 0, so
  its table is there, and a claim hands that BAR over with the rest where
  the machine remaps; the device's `msix=off` leaves it on MSI. The T14's two xHCI functions each publish
  only BAR 0 and armed MSI in the kernel's own bring-up, with no `MSI-X not
  armed` line and one remapping entry each (a `testcases` readback): no MSI-X
  capability came off either walk, so a claim arms MSI and hands BAR 0 over —
  if the walk reached the list's terminator. Whether it did is not read: the
  kernel's own xHCI bring-up, which went with step 10, fell back to MSI on
  `NoCapability::Absent` and `Truncated` alike, and the second refuses the
  claim as `CapsTruncated`. One T14 boot printing which, or the PCI inventory
  printing each function's capability ids and how its walk ended, closes it.
