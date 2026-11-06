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

**Sighting, 2026-11-05: `i219_msi`'s burst does not fill the T14's ring
either.** Its traffic phase queues `4 * TX_RING` frames back to back and
then requires `tx_full` and a taken wake, and on the bench's link the wire
sends them faster than the driver queues them: "the ring full 0 time(s), 0
wake(s) asked and 0 taken, 0 stranded; the MAC counted 65 sent", both on
#1188's T14 run, where the job failed first on its receive check
(https://github.com/ToyOSOrg/evidence/blob/72d7fa82557c6680fb33008665e717ddc83cb849/pr-1188/r3/t14-kernel-excerpt.log,
line 43), and on #1193's B arm, where it is the job's failure
(https://github.com/ToyOSOrg/evidence/tree/3bfbd7de150de5686df1edc749efc33b07722354/pr-1193/r1).
That phase's `tx_full` requirement is this exit's premise and the burst does
not establish it.

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
