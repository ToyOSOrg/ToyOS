---
status: open
kind: tooling
opened: 2026-10-08
---

# The Intel NIC's transmit room and its wake have been read on no hardware

The `i219` driver takes a frame off its link only when it counts room for it
in the part's ring, and where there is none it sleeps on the claim until
`toyos_i219::I219::wake_on_room`'s cause arrives (`lib/link/src/driver.rs`).
On the I219 and the 82574 every reading of that path is the driver's own
model (`system/i219/driver/src/stub.rs`), which is Intel's documents written
down: the T14's part and QEMU's `e1000e` now carry links, but no run has
filled either one's transmit ring, so neither has run it.

What the documents do not say of the T14's part, and the model therefore takes
on trust: that the PCH's MAC raises a message for `TXDW` once it is unmasked.
Its function has an MSI capability and no MSI-X one (631120 §8.1.15), so the
82574's §7.4.1 is the mapping the driver assumes; 631120 publishes no
interrupt register of that MAC at all.

A wake that never comes is no wedge and no lost frame: room is counted from
the descriptors in memory, so the next pass any other event begins finds it.
It is a stall until that pass, and it reads as `transmit.wake_armed` ahead of
`transmit.wake_taken`: the driver counts a wake only on a pass a message
began whose one unmasked cause was the transmit one, so a cause the part
recorded and raised nothing for is never counted.

The same boot is the reading of `TXDCTL` as the bring-up writes it, with bit 22
set as 82574 §10.2.6.11 requires and a write-back per descriptor:
`descriptors.sent` short of `wire.sent`, or armed ahead of taken, makes the
write-back policy this path's.

What either part does with its ring across a link change is not in this
exit: `issues/no-machine-has-read-an-intel-nic-across-a-link-change.md`.

## Owner

The T14's outbound rows of `issues/toyos-has-its-own-network-stack.md`: the
first that fills the wired card's transmit ring.

## Exit

A T14 boot over the `i219` driver's link queues more than
`toyos_i219::TX_RING - 1` frames in one pass, from more sockets than one and
to a destination that answers none of them, and `inspect` then answers
`transmit.full`, `transmit.wake_armed` and `transmit.wake_taken` each at least
1, `descriptors.stranded` 0, and `descriptors.sent`
equal to `wire.sent`; the boot's log carries no "cannot be driven on" line
from the driver. A boot whose `transmit.full` reads 0 never filled the ring and
has read nothing.
