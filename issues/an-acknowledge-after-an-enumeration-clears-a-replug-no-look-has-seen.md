---
status: open
kind: defect
opened: 2026-09-27
---

# An acknowledge after an enumeration clears a replug no look has seen

`device::finish`, `device::refuse` and `device::refuse_for_now`
(`kernel/src/drivers/xhci/device.rs`) report the port with
`PortState::enumerated` and then call `acknowledge_port_read`, which writes
back every change flag the read found, CSC included. `wait::boot::scan_ports`
does the same for every port once the boot scan is over
(`acknowledge_port_changes`).

A replug that lands after the port's last look and before that acknowledge is
cleared before any step reads it. One way in: its Port Status Change Event is
drained by the same `poll` whose `advance_outstanding` ends the enumeration, so
`service_ports` runs after `finish` has written CSC back. The port then reads
connected with no CSC, which is what the driver believes, and the device now in
it is never torn down or enumerated. The look `PortState::believe` owes the
port catches a pull, because CCS reads 0, and not a replug.

**Not measured.** No test stages it. The simulator's `enumerated`
(`toyos-xhci/sim/src/driver.rs`) acknowledges nothing, so no host test can see
it either.

**Owner**: the xHCI driver, `kernel/src/drivers/xhci/`.

**Exit**: the simulator's report acknowledges exactly what the kernel's does,
and a sim test that replugs the device between an enumeration's last answer
and the next look expects `ToreDown(Replugged)` and a second enumeration. That
test is red on the kernel's acknowledge and green without it.
