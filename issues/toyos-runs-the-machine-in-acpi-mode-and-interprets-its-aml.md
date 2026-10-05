---
status: open
kind: track
opened: 2026-10-03
---

# ToyOS runs the machine in ACPI mode and interprets its AML

The T14's firmware hands the machine over in legacy mode, which interrupts
every CPU every 2.2 s
(`issues/the-t14s-firmware-interrupts-every-cpu-every-2-2-s-under-toyos.md`).

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
  (`issues/toyos-explains-itself.md`), not as a one-off check.
- **"Back to legacy mode"**: when the server dies and its `acpi` claim is
  released, the kernel writes `ACPI_DISABLE` to `SMI_CMD`, so the firmware
  handles the buttons again.
- **"Extracts only"**: the repository holds small decoded extracts of the
  T14's ACPI tables; the whole tables stay out of the tree, read only by a
  check run outside it.
- **"Local only; decode the 186 bytes"** (2026-10-04): "Full tables stay out
  of the repo (a copy you hold); the 186-byte fixture on main is replaced by
  decoded values, as 'Extracts only' says." The T14's root-bridge list is
  `t14_root_bridge` in `toyos-acpi/tests/common/mod.rs`.
- **"full clean room write with the spec"**: the AML interpreter is written
  from the ACPI specification.

**Ruled** (owner, 2026-10-04), on the interpreter:

- **"Like Windows, not Linux"**, on `_OSI`: "Yes to every published Windows
  version string, no to 'Linux' and 'FreeBSD', as Linux itself answers. The
  T14 then runs the path it was tested on: Modern Standby, CPU performance
  tables, 101-step backlight, thermal profiles, all devices present."
- **"Yes, one path"**, on power-off: "Power-off always goes through the ACPI
  server; the kernel's power-off table reader is deleted. If the server is
  broken, power-off fails loudly in every test."

The orchestrator's reading of the clean-room ruling, not his: uACPI and
ACPICA are run only as black-box oracles, and whoever writes the interpreter
never reads their source.

**Stage 1: ACPI mode, its SCI served in userland.** The switch to ACPI mode,
and a userland server that claims the SCI, handles the power button, a
fixed event that needs no AML, and takes the EC's events. **Exit**: on the
T14, `MSR_SMI_COUNT`, read through the general counters and not by a check of
its own, stays flat on every CPU over the interval the firmware issue's exit
defines; a press the server serves stops the machine cleanly, through ToyOS's
own power-off path (`SYS_SHUTDOWN`), and the boot's log records the press and
that stop, and each EC query number once with its count: a T14 row reads them
there. A second T14 row kills the server and reads `SCI_EN` clear in `PM1_CNT`
afterwards, the kernel having written `ACPI_DISABLE` to `SMI_CMD`. That every
press of the power button stops the machine is not stage 1's: the T14 loses
some first presses, and
`issues/the-t14s-power-button-event-came-up-to-17-s-after-ec-query-0x28.md`
carries that into the AML interpreter's stages, which follow this one (owner,
2026-10-05, "Land it, record the gap": "The AML stage closes it").

**Ruled** (owner, 2026-10-04, "Stopgap, delete later"): "Stage 1 uses the
extra table so the T14 switches to ACPI mode now." Stage 1 reads the
embedded controller from the ECDT, and that path is a stopgap: it is deleted
the day the interpreter reads the controller from the DSDT's own device. A
machine without an ECDT stays in legacy mode until then; such a machine keeps
its firmware interrupts, and this stage's exit cannot be met on it.

Stage 1's design, not a ruling: a machine whose power button is a control
method device stays in legacy mode too, refused by name; and a machine its
firmware hands over in ACPI mode is served whatever it has, since nothing is
written.

**Stage: power-off through the server** (the orchestrator's placement of "Yes,
one path"). The ACPI server evaluates `\_S5` and powers the machine off.
Blocked on the interpreter's evaluation of `\_S5`. **Exit**: the kernel's `\_S5_` reader, `toyos-acpi/src/dsdt.rs`, and its caller
in `kernel/src/arch/x86_64/power.rs` are deleted, and a test that powers off
through a broken server is red.
