---
status: open
kind: defect
opened: 2026-09-28
---

# A poll that leaves a port outstanding with no wake says nothing

The simulator's pump (`toyos-xhci/sim/src/driver.rs`) fails with
`Stuck::Unwoken` when a pass leaves the port outstanding
(`PortState::outstanding`) with no instant to come back at. The kernel has no
counterpart. `XhciController::poll` (`kernel/src/drivers/xhci/mod.rs`) returns
`None` in that state, `poll_if_pending` stores 0 in `PORT_WORK_AT`, and the port
is not read again until some other xHCI interrupt arrives. On hardware nothing
names the state, and the simulator catches it only on the arms it models.

**Owner**: the xHCI driver, `kernel/src/drivers/xhci/`.

**Exit**: `poll` logs a line naming the port whenever it returns `None` with a
port outstanding.
