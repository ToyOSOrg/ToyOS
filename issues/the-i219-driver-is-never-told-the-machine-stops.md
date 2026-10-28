---
status: open
kind: defect
opened: 2026-10-28
---

# The I219 driver is never told the machine stops

The supervisor tells only disk drivers that the machine stops
(`toyos_quiesce::DRIVER_STOP`, `stop_drivers` in
`system/supervisor/src/main.rs`). The `i219` driver is a `drives` row the
supervisor gives no stop pipe, so a reboot or power-off parks it in the
kernel's quiesce and its I219 destructor never runs: `CTRL_EXT` bit 28, the
host's word to the firmware that a driver holds the function (`toyos_i219`'s
`pch`), stays set across the reset, and nothing the driver could say about
the PHY at the end reaches the log. The disk drivers' stop pipe is the
mechanism a `drives` row already has for it.

The next operating system's start does not depend on it: the driver disarms
Ultra Low Power at its own start (`system/i219/driver/src/ulp.rs`), and a
machine cut off mid-boot gets no stop either.

**Owner**: stage 3 of `issues/the-supervisor-is-host-tested-and-owns-the-stop.md`,
where the supervisor asks each service it started to finish before it calls
the kernel.

**Exit**: a T14 reboot whose log shows the I219's driver told of the stop, its
destructor run and `CTRL_EXT` bit 28 clear before the kernel's reset.
