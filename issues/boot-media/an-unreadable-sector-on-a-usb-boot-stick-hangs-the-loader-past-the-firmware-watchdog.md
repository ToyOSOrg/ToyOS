---
status: open
kind: defect
opened: 2026-09-28
---

# An unreadable sector on a USB boot stick hangs the loader past the firmware watchdog

On stock edk2, a ROOT sector the USB boot stick fails with EIO stops the boot
inside the loader's read of ROOT. The read did not return in 147 s, and the 60 s
watchdog the loader arms at entry did not reset the machine.
`bootloader/src/rootimage.rs`'s `read_root` leans on that watchdog "if the
firmware honours it". This firmware did not.

Measured on the dev host with Homebrew QEMU 11.1.1's own
`edk2-x86_64-code.fd` under TCG. The stimulus is `root_chunk_refused`'s as it
stood on the Headless profile: `blkdebug` failing every `read_aio` covering
the sector seven past ROOT's middle with errno 5, under the boot image on a
`usb-storage` on `nec-usb-xhci`. The harness's deadline was lifted to 150 s,
and each 16550 line was stamped with the seconds since QEMU's spawn:

- `Firmware watchdog: 60 s, until ExitBootServices disables it` at 2.0 s;
- `Slot A: signed header … verifies under this loader's key` at 3.0 s. This
  is the last byte on the 16550. The loader's next step is the chunked read
  of ROOT that covers the failing sector;
- no further byte in the 147 s to the deadline, no `the read of … failed`
  line, and no reset. `-no-reboot` would have turned a reset into QEMU's
  exit, which the harness reports as `QEMU died before`. It reported
  `Boot timed out` instead.

The test was written against the tree's former `ovmf/` image, and it asserts
`DEVICE_ERROR` from this same read.

This measurement does not show which side does not finish: edk2's USB
mass-storage and xHCI stack, or QEMU's `usb-storage` answer to a failed data
phase. It also does not show why the watchdog's timer event did not run.
`usb_pcap` records only the first data disk and not the boot stick, so no
existing instrument sees the bus here.

`root_chunk_refused` now stages the same EIO on the `InternalDisk` profile's
NVMe boot disk. Nothing covers the loader's refusal of a ROOT chunk off a USB
stick.

## Exit condition

On stock edk2, a guest test that stages this EIO under ROOT on a USB boot stick
ends within `FIRMWARE_WATCHDOG_SECS` in the loader's named refusal of the chunk
or in a reset. Then this file is deleted.

## Owner

`bootloader/src/rootimage.rs`'s `read_root` and the loader's watchdog in
`bootloader/src/main.rs`; unheld.
