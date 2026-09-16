---
status: open
kind: defect
opened: 2026-09-13
---

# A BAR is placed in a window firmware names no part of

`pcidev::publish` derives the two windows it moves a claimed function's BARs
into from what firmware *used* — every memory-map entry and every assigned BAR —
and puts them on the next 2 MiB boundary above all of it. Nothing in that
derivation asks where the root bridges decode, so on both machines this project
boots the window lands outside the aperture firmware named, and the kernel moves
a BAR there anyway.

The kernel now says so on every boot (`pcidev: the <width> window ... is inside
no window firmware named`), and that record is the whole of what it does about
it.

Measured. ThinkPad T14, metal run 34 (`kernel.log:71-72`):

    pcidev: 24 functions; a 32-bit window comes from 0x0..0x0, a 64-bit one from 0x603dc00000..0x6040c00000
    pcidev: firmware root bridge windows: mem 0xa2000000..0xbd000000, mem 0x4000000000..0x603dc00000

QEMU 11.1.0 q35 under `ovmf/OVMF_CODE-pure-efi.fd`, headless, 4 GiB:

    pcidev: 5 functions; a 32-bit window comes from 0xc0200000..0xc3200000, a 64-bit one from 0x800200000..0x803200000
    pcidev: firmware root bridge windows: mem 0xc0000000..0xc0100000, mem 0x800000000..0x800100000

Both placement windows are wholly outside both answers on both machines. That
QEMU boots regardless is q35 routing everything above its RAM to PCI at priority
-1, which is a property of that machine and not a rule; the T14 is where an
unrouted window reads all-ones, and `Refusal::Dead` cannot tell that from an
absent device.

**Refusing the window is not the fix and must not be done on its own**: every
window on every machine in reach is outside the answer, so a kernel that refused
one would hand over no function anywhere, and `pci_function_is_exclusive`,
`virtio_net_no_msix` and `iommu_virtio_platform` would all go red on a defect
they are not about.

Exit condition: the windows are *derived* from the aperture — the firmware
windows intersected with the address space nothing already decodes — so that
what is placed is inside one by construction, and the record above becomes a
refusal because it can no longer fire on a machine that works. The protocol
answers a bridge's current settings and not the platform's whole aperture, so
that derivation has to answer what a machine whose answer leaves no 2 MiB run
gets: the T14's answer leaves one run (`0xae200000..0xb0000000`), and OVMF's
one-megabyte hull leaves none.
