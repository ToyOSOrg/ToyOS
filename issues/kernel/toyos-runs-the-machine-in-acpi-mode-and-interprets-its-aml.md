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

**Ruled** (owner, 2026-10-03), on stage 1:

- **Each embedded-controller event is taken off the controller.** The log gets
  a line the first time a query number appears and a count at intervals, never
  a line per event: under Linux the T14's EC raises about 2.6 a second (#682,
  comment 5968053374).
- **The server holds the EC's ports unfiltered.** The kernel filters no EC
  command; what the holder can do with those ports is recorded as a weakness
  in an issue of its own, as #592 records the i8042 holder's reset line.
- **Stage 1 is built on #592's `isa` claim**, as one row the FADT and the ECDT
  name, after #592 lands.
- **The attended press waits for the owner.** Everything else in stage 1 is
  built and reviewed first; he then presses the button once, briefly, on a
  boot held open for it.

**Ruled** (owner, 2026-10-03), on the server, the tables and the interpreter:

- **"General counters"**: stage 1's `MSR_SMI_COUNT` reading is built as the
  first piece of the general counters
  (`issues/diagnostics/toyos-explains-itself.md`), not as a one-off check.
- **"Back to legacy mode"**: when the server dies and its `acpi` claim is
  released, the kernel writes `ACPI_DISABLE` to `SMI_CMD`, so the firmware
  handles the buttons again.
- **"Extracts only"**: the repository holds small decoded extracts of the
  T14's ACPI tables; the whole tables stay out of the tree, read only by a
  check run outside it.
- **"full clean room write with the spec"**: the AML interpreter is written
  from the ACPI specification.

The orchestrator's reading of the clean-room ruling, not his: uACPI and
ACPICA are run only as black-box oracles, and whoever writes the interpreter
never reads their source.

**Stage 1: ACPI mode, its SCI served in userland.** The switch to ACPI mode,
and a userland server that claims the SCI, handles the power button, a
fixed event that needs no AML, and takes the EC's events. **Exit**: on the
T14, `MSR_SMI_COUNT`, read through the general counters and not by a check of
its own, stays flat on every CPU over the interval the firmware issue's exit
defines, and a press of the power button stops the machine cleanly, through ToyOS's own
power-off path (`SYS_SHUTDOWN`), and the boot's log records the press and that
stop, and each EC query number once with its count: a T14 row reads them there.
A second T14 row kills the server and reads `SCI_EN` clear in `PM1_CNT`
afterwards, the kernel having written `ACPI_DISABLE` to `SMI_CMD`.

**Open, the owner's to decide**: whether a machine whose firmware publishes no
ECDT is put in ACPI mode. Stage 1 refuses it by name and leaves it in legacy
mode, as it leaves a machine whose power button is a control method device:
in legacy mode its firmware serves its embedded controller and its button,
and in ACPI mode nothing would until the interpreter does. The cost: such a
machine keeps its firmware interrupts, and this stage's exit cannot be met on
it. A machine its firmware hands over in ACPI mode is served whatever it has,
since nothing is written.
