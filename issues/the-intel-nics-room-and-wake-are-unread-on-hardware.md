---
status: open
kind: tooling
opened: 2026-10-08
---

# The Intel NIC's transmit room and its wake have been read on no hardware

netstack offers its card a frame only when the driver counts room for it, and
where there is none it sleeps on the claim until `toyos_i219::I219::wake_on_room`'s
cause arrives. On the I219 and the 82574 every reading of that path is the
driver's own model (`toyos-i219/src/stub.rs`), which is the 82574 datasheet
written down: no image gives netstack `pci:8086:15fc`, and the harness has no
Intel card, so neither the T14 nor QEMU's `e1000e` has run it.

What the model cannot say of the T14's part: which of the cause's two names
(`TXDW`, or `TXQ0` through `IVAR`) the PCH's MAC raises a message for, and
whether a ring left full across a link going down is written back when the
link returns.

A wake that never comes is no wedge and no lost frame: room is counted from
the descriptors in memory, so the next pass any other event begins finds it.
It is a stall until that pass.

## Owner

The T14's outbound rows of `issues/toyos-has-its-own-network-stack.md`: the
first boot that gives netstack the wired card.

## Exit

A T14 boot whose netstack drives `8086:15fc` sends more than
`toyos_i219::TX_RING - 1` frames in one pass, and its readback shows the pass
after the full ring begun by the card's interrupt and every frame sent
(`descriptors.sent` against `wire.sent` in `inspect`).
