---
status: open
kind: defect
opened: 2026-10-28
---

# netstack is never told the machine stops

The supervisor tells only disk drivers that the machine stops
(`toyos_quiesce::DRIVER_STOP`, `stop_drivers` in
`system/supervisor/src/main.rs`). netstack is a `[boot] start` service holding
its card through `devices`, so a reboot or power-off parks it in the kernel's
quiesce and its I219 destructor never runs: `CTRL_EXT` bit 28, the host's word
to the firmware that a driver holds the function (`toyos_i219`'s `pch`), stays
set across the reset, and nothing the driver could say about the PHY at the end
reaches the log.

The next operating system's start is not left depending on it: the driver
disarms Ultra Low Power at its start (`system/netstack/i219/src/ulp.rs`), so
the PHY it leaves is the one a machine cut off mid-boot leaves too.

**Owner**: `issues/a-device-function-is-driven-from-its-arrival-to-its-end.md`,
whose ruling makes the NIC driver one process per function.

**Exit**: a T14 reboot whose log shows the I219's driver told of the stop, its
destructor run and `CTRL_EXT` bit 28 clear before the kernel's reset.
