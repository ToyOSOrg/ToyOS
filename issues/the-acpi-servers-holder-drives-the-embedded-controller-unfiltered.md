---
status: open
kind: defect
opened: 2026-10-04
---

# The ACPI server's holder drives the embedded controller unfiltered

The holder of the `acpi` claim, `/system/bin/acpiserver`, reaches the
embedded controller's command and data ports through the kernel's mediated
access, one byte each (`access`, `kernel/src/arch/x86_64/acpi_mode.rs`;
`system/acpiserver/src/host.rs`). No row names those ports, and the fence
(`toyos_userbound::firmware`) refuses a port only for whose it is, never for
the value written. The server sends queries and, for the AML's accesses to
the controller's space, `RD_EC` and `WR_EC` (`system/acpiserver/src/ec.rs`).
`WR_EC` (ACPI 6.5 §12.3) writes any byte of the controller's space: on a
laptop that is where fan control, charge thresholds and the controller's own
thermal limits live. So a bug in that one program, or in the AML it runs, can
reach whatever the machine's controller lets its host write, and the kernel
makes the write without judging it.

The command port also takes any byte, and §12.3 defines five commands
(0x80–0x84); every other is the controller's maker's. Whether one of them puts
the T14's controller into a firmware update has not been read. If one does, a
bug in the holder can rewrite the controller's firmware, which outlives the
holder and the boot: the escape
`issues/a-driver-that-reflashes-its-device-escapes-its-isolation.md` records
for a driver's device ("Refuse external, record reflash", owner, 2026-10-04).

**Ruled** (owner, 2026-10-03, stage 1 of
`issues/toyos-runs-the-machine-in-acpi-mode-and-interprets-its-aml.md`):
the server holds the ports unfiltered, and this is the weakness recorded. That
track owns it.

**Exit**: what the holder writes to the controller is bounded by something
other than the holder, as `issues/the-i8042s-holder-holds-the-machines-reset-line.md`
asks of the i8042's reset line.
