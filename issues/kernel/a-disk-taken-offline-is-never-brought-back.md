---
status: open
kind: defect
opened: 2026-09-16
---

# A disk taken offline is never brought back

When a mass-storage device's transport breaks `MAX_TRANSPORT_BREAKS` times
running, or its Reset Recovery fails, `msc::take_offline` stops its endpoints,
resets its port, gives its slot back and refuses every later operation on it. The device is at its
Default state on a powered port, which is what the next host needs; this boot
has lost the disk, and on the T14 that disk is the root filesystem, so the boot
is over from there.

The specifications define one more rung, which this driver does not climb.
The port reset `take_offline` already issues leaves the device in its Default
state, and xHCI 1.2 §4.6.11's Reset Device Command is what the host then owes
the slot of a device that was reset: it sets the Slot State to Default and the
USB Device Address to 0, and disables every endpoint but the default control
one. Address Device (§4.6.5),
SET_CONFIGURATION and Configure Endpoint (§4.6.6) then make the same device,
now clean, one that can be spoken to again. What makes that
worth doing is keeping the disk's identity: `usb_storage::handle` indexes by
the number `bind` handed out, a mount holds it for life, and today
`release_blocks` says a number never comes back. A device reset that re-binds
under the same index keeps the mounts, and a stick that hiccupped costs a
few hundred milliseconds instead of the boot.

Not done in the change that added the offline path because it is a second
enumeration path through `device.rs` — the existing one hands out a new slot
and a new index — and because the device that would measure it is the T14's
stick, which the offline path was the first step towards not wedging.

## Exit condition

A QEMU boot under `usb-transport-faults` in which the third refused CBW is followed by a
device reset, the disk re-binds under its original index and the gate's later
reads succeed; and a T14 boot whose stick stalled mid-boot that finishes with
its mounts intact.
