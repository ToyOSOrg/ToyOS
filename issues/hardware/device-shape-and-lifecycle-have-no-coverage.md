---
status: open
kind: track
opened: 2026-08-02
---

# Device shape and lifecycle have no coverage, and shape comes before protocol depth

Almost every device test in the estate runs against one machine shape, with
every device present and none arriving or leaving. The exception is USB: the
harness speaks QEMU's own `device_add`/`device_del` over QMP
(`tests/common/qemu.rs:3472-3500`), and `xhci_hotplug` is one of the arms that
use it (`tests/common/usb.rs:2304`, registered at `tests/toyos.rs:923`,
dispatched at `:8628`). One bus is not a matrix. The ground truth a device test owes
is at the hardware boundary: what the guest did to the device, read back from
the device — a captured wav, a pcap of the virtual wire, an image decoded
host-side with the kernel's own parser — not what the guest said it did.

**Order, and it is deliberate: shape and lifecycle before protocol depth.** A
daemon whose device is absent must exit cleanly — no panic, and no holding a
service name with nothing behind it — and that is worth more than another layer
of protocol conformance.

1. **Shape matrix.** Parameterise the profile machinery over device sets:
   no-USB-HID, four-USB-devices, no-NVMe, hotplug-after-boot, remove-under-load.
   Assert the boot reaches the compositor and that the right daemons live or
   exit. Two shapes exist (the T14's six-device xHCI, and its exact NVMe
   namespace); the rest do not.
2. **Lifecycle, on one bus.** `QmpDevices` is built
   (`tests/common/qemu.rs:3479-3510`) and twelve sites use it: eleven in
   `tests/common/usb.rs` — hot-plugged mice, keyboards and `usb-storage` behind
   a `blockdev_add`, including a replug — and one compositor churn arm in
   `tests/toyos.rs:12269-12288` that unplugs a mouse while it is delivering
   motion. Every `add` names `xhci.0` or `xhci1.0`; nothing outside USB arrives
   or leaves. Still unbuilt: removal under active *storage* I/O, and
   claim-then-die-then-reclaim.
3. **Storage ground truth.** Started — one test writes a file in-guest, shuts
   down, and decodes both superblocks out of the image host-side with the
   kernel's own parser, so one assertion covers write-back and capacity at once.
   Remaining: the file's own bytes, which needs a file-backed `BlockIO` so the
   harness can walk the btree rather than only the superblocks.
4. **The network gate** — `issues/build/there-is-no-network-gate.md`.
