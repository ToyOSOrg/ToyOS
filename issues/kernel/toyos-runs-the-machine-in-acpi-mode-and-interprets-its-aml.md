---
status: open
kind: track
opened: 2026-10-03
---

# ToyOS runs the machine in ACPI mode and interprets its AML

ToyOS leaves the machine in the mode its firmware hands over: nothing writes
`ACPI_ENABLE` to `SMI_CMD`, and nothing handles an SCI. The T14's firmware
hands it over in legacy mode, which interrupts every CPU every 2.2 s
(`issues/hardware/the-t14s-firmware-interrupts-every-cpu-every-2-2-s-under-toyos.md`).

Nothing in the tree evaluates the AML in a machine's DSDT or SSDTs. What
ToyOS takes from them it takes another way: `toyos-acpi/src/dsdt.rs` finds
`\_S5_` by a byte scan, and the loader asks UEFI for the root bridges'
windows `_CRS` would name (`bootloader/src/rootbridge.rs`). Nothing reads
`_CST`, which names a CPU's C-states.

**Ruled** (owner, 2026-10-03, on the scout in #681's comment 5966817345): the
switch to ACPI mode lands as stage 1, together with a userland server that
claims the SCI and handles the power button, so nothing the firmware does in
legacy mode today is lost; not the switch alone with the events masked, and
not after the AML work. The AML interpreter follows as later stages: ToyOS
writes its own, and the battery comes first (his direction of `0ee814f5a`).

**Stage 1: ACPI mode, its SCI served in userland.** The switch to ACPI mode,
and a userland server that claims the SCI and handles the power button, a
fixed event that needs no AML. **Exit**: on the T14, `MSR_SMI_COUNT` stays
flat on every CPU over the interval the firmware issue's exit defines, and a
press of the power button stops the machine cleanly, through ToyOS's own
power-off path (`SYS_SHUTDOWN`), and the boot's log records the press and that
stop: a T14 row reads both there.
