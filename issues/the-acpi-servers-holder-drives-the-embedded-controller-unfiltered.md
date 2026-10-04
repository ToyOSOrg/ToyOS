---
status: open
kind: defect
opened: 2026-10-04
---

# The ACPI server's holder drives the embedded controller unfiltered

The `acpi` claim (`kernel/src/arch/x86_64/acpi_mode.rs`) opens the embedded
controller's command and data ports to `/system/bin/acpiserver` through the
TSS I/O permission bitmap, which grants a port or refuses it and cannot see
the value written. Stage 1 sends the controller nothing but queries, but the
same two ports take `WR_EC` (ACPI 6.5 §12.3), which writes any byte of the
controller's space: on a laptop that is where fan control, charge thresholds
and the controller's own thermal limits live. So a bug in that one program can
reach whatever the machine's controller lets its host write, which the kernel
never sees.

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
