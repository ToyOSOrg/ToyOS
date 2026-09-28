---
status: open
kind: defect
opened: 2026-09-28
---

# A device swapped into its port inside the port rung's reset is taken for the disk it replaced

Derived from the code, not staged.

The port rung (`port_reset_recovery` in `kernel/src/drivers/xhci/wait/msc.rs`)
resets the port and reads it once (`reset_port`). When
`toyos_xhci::ladder::after_reset` answers `Enumerate`, the port reads
connected, enabled and at the speed the disk was bound at. The rung then
addresses the disk's own slot again, sets its old configuration, adds its old
bulk pair and asks TEST UNIT READY. **Nothing re-reads who the device is.**

Take a device pulled after the reset completes and another plugged into the
same port before that read. On a USB3 port the new device trains to Enabled by
itself (§4.19.1.2). The CSC its arrival raised is the one
`enumeration_ack(Some(Warm), …)` spends, because a warm reset's retrain raises
one too (§4.19.5.1) and nothing tells the two apart. A second unit of the same
model answers every step: every identity field is the same but its serial
number, as with `usb_transport_break`'s `AnotherStick`. The rung then carries
disk 0's volume on onto it.

The port machine does not see the swap either. Its belief stays attached with
disk 0's slot, and the rung's acknowledge spent the edge.

`usb_transport_break` moves the stick to port 3, so the bind's serial check
(`XhciController::adopt`) judges it there. Nothing stages the same port.

**Exit**: a disk the port rung took back is the device it was. Its serial
number is read again and judged by `toyos_xhci::identity::same` before the
volume carries on. A `usb-reset-moves` staging that plugs another serial number
into the same port refuses it by name.
