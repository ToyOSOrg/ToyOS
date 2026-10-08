---
status: open
kind: tooling
opened: 2026-10-08
---

# The Intel NIC's transmit room and its wake have been read on no hardware

netstack offers its card a frame only when the driver counts room for it, and
where there is none it sleeps on the claim until `toyos_i219::I219::wake_on_room`'s
cause arrives. On the I219 and the 82574 every reading of that path is the
driver's own model (`toyos-i219/src/stub.rs`), which is Intel's documents
written down: no image gives netstack `pci:8086:15fc`, and the harness has no
Intel card, so neither the T14 nor QEMU's `e1000e` has run it.

What the documents do not say of the T14's part, and the model therefore takes
on trust: that the PCH's MAC raises a message for `TXDW` once it is unmasked.
Its function has an MSI capability and no MSI-X one (631120 §8.1.15), so the
82574's §7.4.1 is the mapping the driver assumes; 631120 publishes no
interrupt register of that MAC at all.

A wake that never comes is no wedge and no lost frame: room is counted from
the descriptors in memory, so the next pass any other event begins finds it.
It is a stall until that pass, and it reads as `transmit.wake_armed` ahead of
`transmit.wake_taken`.

Not in the exit: the reset that takes the transmit ring back when the link
changes over unsent frames. It needs a link to drop under traffic, which no
row can stage. A boot where it happens says so by itself: netstack's line
"the link changed over unsent frames", and `descriptors.unsent` in `inspect`.

## Owner

The T14's outbound rows of `issues/toyos-has-its-own-network-stack.md`: the
first boot that gives netstack the wired card.

## Exit

A T14 boot whose netstack drives `8086:15fc` sends more than
`toyos_i219::TX_RING - 1` frames in one pass, and `inspect` then answers
`transmit.full`, `transmit.wake_armed` and `transmit.wake_taken` each at least
1, `descriptors.unsent` 0, and `descriptors.sent` equal to `wire.sent`; the
boot's log carries no "cannot be driven on" line from netstack.
