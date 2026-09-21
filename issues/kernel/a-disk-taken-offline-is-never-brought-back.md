---
status: open
kind: defect
opened: 2026-09-16
---

# A disk taken offline is never brought back

A mass-storage device whose transport breaks climbs `toyos_xhci::ladder`: the
class reset, then a port reset after which the device is addressed and
configured again on the slot, the pool block and the disk number it already
has, so a mount on it carries on. A device that does not answer after the port
reset is taken offline behind one more reset, its slot goes back, and every
later operation on it is refused until it is unplugged. On the T14 that disk is
the root filesystem, so the boot is over from there.

Two ways a disk that could still be served is lost today:

- **The port reset moves it.** T14 run 74: a SuperSpeed stick enumerated on the
  USB2 half of its receptacle answered the bus reset by training SuperSpeed on
  the other half, so it left port 1 and arrived on port 13. The rung reads its
  port disconnected, the port's teardown takes the disk, and the device that
  arrives is bound under a new number that no mount holds.
- **Offline is for the connection's life.** Nothing asks an offline device
  again, however long it stays plugged in.

Both come to the same missing piece: a mount holds a disk number for life
(`usb_storage::handle`, `release_blocks`), and nothing re-attaches a number to
a device that is the same device.

## Exit condition

A T14 boot whose stick's transport broke mid-boot and that finishes with its
mounts intact, on either half of the receptacle; and a disk that left its port
under a reset and arrived on another keeps its number.
