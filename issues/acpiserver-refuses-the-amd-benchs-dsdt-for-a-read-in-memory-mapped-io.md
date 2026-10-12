---
status: open
kind: defect
opened: 2026-10-11
---

# acpiserver refuses the AMD bench's DSDT for a SystemMemory read in memory-mapped I/O, so its `\_S5` is never handed over

On the AMD bench's boot at `fb03703a1` (PR #852, the log in
issuecomment-6107031259) the DSDT's load makes one SystemMemory read, a Byte,
at an address the firmware's map types 11, `EfiMemoryMappedIO`. The kernel
refuses it `MemoryType`: `toyos_userbound::firmware`'s `decide` passes memory
of types 0, 6, 9 and 10 and, to read, an address the map does not list, and
refuses every other type. The server says so by name:

```
acpiserver: table 1 of 17 (DSDT) refused: denied by this server: a SystemMemory read the kernel refused MemoryType, in memory of type 11
acpiserver: no namespace: the DSDT was refused, so the 16 SSDT(s) after it are not loaded; the power button is served, and nothing of this machine's AML
acpiserver: 0 of 17 tables loaded in 20 ms
acpiserver: no \_S5 was handed to the kernel, which powers off only on one an ACPI server supplied and refuses a shutdown without one
```

So that bench has no power-off, and, its button being a control method
device, no button: the owner's "Stay in ACPI mode, button dead" is the ruled
state for a machine whose namespace was not kept
(`issues/toyos-runs-the-machine-in-acpi-mode-and-interprets-its-aml.md`). The
address itself the server marks as the machine's own and quoted in no
record, and it is not quoted here. The same load's two SystemIO index writes
passed, as #845 measured them.

A register a firmware types `EfiMemoryMappedIO` is a device's, which the
kernel's present bound does not reach; what AML may read and write in it, and
checked against what, is the policy owed. It is the case of
`issues/the-acpi-claims-holder-reaches-every-port-and-firmware-range-the-kernel-did-not-declare.md`,
whose exit asks that an access pass only inside a region the machine's loaded
tables define, which a DSDT refused at its load never defines.

Owner: the interpreter stage of
`issues/toyos-runs-the-machine-in-acpi-mode-and-interprets-its-aml.md`; the
widening of the kernel's bound is a security boundary and is ruled before it
is built.

## Exit condition

The policy for AML's SystemMemory OperationRegions in memory the firmware
types `EfiMemoryMappedIO` is ruled, `firmware::decide` holds to it with a
host test of each arm, and an AMD bench boot reads all 17 tables loaded and
`\_S5` handed to the kernel; or the ruling refuses such reads, and this bench's
lost power-off is recorded as the ruled state.

**Ruled** (owner, 2026-10-12, on #875): such a read is a register's, read
as an unlisted one is (comment 6120173372), built in #875 with a host test
of each arm; and SSDT8 is refused whole (comment 6121646183,
`issues/the-amd-benchs-ssdt8-scopes-a-controller-no-table-defines.md`), so
the boot's reading is 16 of 17 tables loaded, SSDT8 the one refused, and
`\_S5` handed: the bench's `acpi_tables_loaded` reads it.
