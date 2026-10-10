---
status: open
kind: defect
opened: 2026-10-09
---

# A HID interface outside the boot subclass is read as QEMU's tablet

`kernel/src/drivers/xhci/device.rs`'s `parse_config` binds every HID
interface of subclass 0, whatever its protocol, as `HidType::Tablet`, and
`toyos-usbhid`'s `Pointer::Tablet` reads its reports in the layout of QEMU's
`usb-tablet`: buttons, X and Y little endian, the wheel. No report descriptor
is read, so that is true of QEMU's tablet and of nothing else by construction.
A keyboard's second interface carrying its media keys, a mouse left in report
protocol, a security key or a UPS each publishes a subclass-0 interface; its
reports are refused by length or, at six bytes, land as absolute pointer
positions. Nothing in reach has been seen doing it: the T14's own USB devices
bind no HID interface at all (`0 HID device(s)` in a `testcases` readback).

Owned by usbd, the small-kernel track's step 10
(`issues/the-kernel-is-small-interrupts-post-and-threads-wait.md`), which
binds every HID interface from userland.

**Exit condition.** A subclass-0 interface is bound only from its report
descriptor, read by a parser in `toyos-usbhid` that refuses one it cannot
read by name, and a host test feeds it QEMU's tablet descriptor and one of
another layout.
