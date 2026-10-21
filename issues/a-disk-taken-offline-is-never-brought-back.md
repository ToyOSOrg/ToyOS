---
status: open
kind: defect
opened: 2026-09-16
---

# A disk taken offline is never brought back

A mass-storage device whose transport breaks climbs usbd's ladder
(`system/usbd/xhci/src/msc.rs`, over `toyos_xhci::ladder`): the class
reset, then a port reset and enumeration again, its disk held meanwhile for
its return. A device that does not answer after the port reset is taken
offline: the last thing it is sent is a reset, its disk ends `Gone` with every
session on it, and its port keeps it, unasked, until it is unplugged. On the
T14 that disk carries the log volume and the data volume.

- **The port reset may move it.** T14 runs 74 and 79: a SuperSpeed stick
  enumerated on the USB2 half of its receptacle answered the bus reset by
  training SuperSpeed on the other half, so it left port 1 and arrived on port
  13. usbd holds the disk for `identity::RETURN_WINDOW` after its own reset
  and gives it back, sessions and queued work whole, to a device that proves
  the same identity on any port
  (`a_stick_back_from_its_port_reset_keeps_its_disk_and_its_work`, on the
  stub). No bench has measured the move under usbd.
- **Offline is for the connection's life.** Nothing asks an offline device
  again, however long it stays plugged in.

## Exit condition

A T14 boot whose stick's transport broke mid-boot and that finishes with its
mounts intact, on either half of the receptacle; and an offline device asked
again, or the reason it must not be, stated.
