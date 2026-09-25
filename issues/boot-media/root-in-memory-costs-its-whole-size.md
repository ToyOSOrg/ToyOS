---
status: open
kind: finding
opened: 2026-09-25
---

# ROOT in memory costs its whole partition, in RAM and in a read every boot

The loader reads the whole ROOT partition (`bootloader/src/rootimage.rs`)
and the kernel keeps it for the machine's life. `src/image.rs` sizes ROOT to
its contents plus a tenth, so the cost is what the image carries: the
`--build-only` image's ROOT is 117 MiB (29952 blocks), the test suite's is 701 MiB
(171264 blocks) on a 4 GiB guest.

The read, QEMU 11 TCG, host clock and TSC agreeing: 117 MiB in 98–111 ms off
`usb-storage`, 20 ms off `virtio-blk`; the test image's 701 MiB in 547–580 ms
off `usb-storage`, which every USB-booted test guest now pays. Against the
base, that image's time to `Boot: complete` on USB moved from about
1.19 s to 1.29 s of host wall clock, the kernel's own boot unchanged (219–229
ms both).

Not measured: the T14. What that run owes is the read's TSC cycles off the
stick and off NVMe (`ROOT: read into memory at … in N TSC cycles` in
`loader.log`, converted by the kernel's `TSC:` record), and the same image's
time to `Boot: complete` against the base.

Exit: either measured and accepted by the owner as the price, or ROOT's
image trimmed to what it carries (the tenth of slack, and the test image's
payload) before the self-update's signature check makes the whole read a
fixed cost.
