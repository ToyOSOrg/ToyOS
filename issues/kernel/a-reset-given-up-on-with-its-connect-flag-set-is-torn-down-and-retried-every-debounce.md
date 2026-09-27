---
status: open
kind: defect
opened: 2026-09-28
---

# A reset given up on with its connect flag set is torn down and retried every debounce

`PortState::step` (`toyos-xhci/src/port.rs`) gives up on a port whose reset
completed disabled with no escalation left (`GaveUp::LinkNeverTrained`,
`GaveUp::ResetFailed`) and leaves it attached, so that it is not reset again
until its device is pulled. `service_port` (`kernel/src/drivers/xhci/mod.rs`)
and the simulator's pump read the port again in the same pass.

If the completion left CCS and CSC set (§4.19.5.1: a warm completion carries
the retrain's connect edge), that read is a replug. The port is torn down,
debounced and reset again, and gives up again: once per debounce for as long
as the device stays in, which is the loop `GaveUp` exists to stop.

**Not measured.** No `ResetBehaviour` in `toyos-xhci/sim/src/hub.rs` completes
a reset with the port connected and disabled, and QEMU completes every reset
enabled.

**Owner**: the xHCI port machine, `toyos-xhci/src/port.rs`.

**Exit**: a `ResetBehaviour` whose warm reset completes with CCS and CSC set and
PED clear, and a sim test that holds such a device in its port for many
debounces and expects exactly one `GaveUp` and no teardown.
