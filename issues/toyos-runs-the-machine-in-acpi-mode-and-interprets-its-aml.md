---
status: open
kind: track
opened: 2026-10-03
---

# ToyOS runs the machine in ACPI mode and interprets its AML

ToyOS leaves the machine in the mode its firmware hands over: nothing writes
`ACPI_ENABLE` to `SMI_CMD`, and nothing handles an SCI. The T14's firmware
hands it over in legacy mode, which interrupts every CPU every 2.2 s
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

**Ruled** (owner, 2026-10-04, his words as the orchestrator's record of the
session holds them), asked where the specifications and a reference
implementation would be kept: "Nowhere why do we need existing c code ans
why do we need to persist prose. The specs exist we can reference them cant
we? Clean romm is only needed for reading code and writing using that code
in a transferred sense". The orchestrator's reading of it: no other
implementation is kept, as an oracle or otherwise, and no specification is
copied into a repository; whoever writes the interpreter works from the ACPI
Specification itself and never reads another AML implementation's source.

**Ruled** (owner, 2026-10-05), on the interpreter (the option chosen, then
its text, verbatim):

- **`\_OS`**: "\"Microsoft Windows NT\" (Recommended)" — "Same as Windows,
  consistent with 'Like Windows, not Linux': firmware that branches on _OS
  takes the tested Windows path."
- **`_OSI` feature groups**: "Like Windows answers (Recommended)" — "Answer
  each feature group the way Windows does, so the T14 takes its tested path;
  the interpreter must then actually support what it claims."
- **Old opcodes**: "Accept what real firmware ships (Recommended)" — "Parse
  Processor and other legacy constructs real tables still contain, per their
  last spec definition; refuse only what is truly malformed. Tested against
  QEMU's table in the tree and your T14 tables locally."

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

**Stage: the interpreter.** `userland/acpiserver/aml`, the AML
interpreter, pure and host-tested inside the server that uses it. **Exit**:
a host test loads QEMU 11.1.1's DSDT
(`toyos-acpi/fixtures/qemu-11.1.1/dsdt.bin`) and evaluates `\_S5` to the
`SLP_TYPa` its boot logged, 0; and the T14's DSDT and SSDTs, read by a
check run outside the tree, load and evaluate `\_S5`, its pull request
recording the result.

What that check found, which whoever builds on the interpreter would
otherwise pay to find again:

- **The T14's tables load only against its own memory and its own
  bridges.** Their
  definition-block code reads SystemMemory and PCI_Config while it loads, and
  branches on what it reads: with every read answered zero the DSDT refers to
  a device its own other branch never defined, and is refused. The check
  answered one 16-bit word of memory, the chipset series, and nothing else
  of it. One SSDT reads a field below a bridge while it loads, and the
  interpreter refuses a PCI_Config region below a function that is no
  PCI-to-PCI bridge by its Header Type, or whose Secondary Bus Number is not
  above its own bus (`pci`, `userland/acpiserver/aml/src/field.rs`). With
  the bridges answering as present and numbered all 14 tables load; with
  them answering zero, as bridges at reset, or as absent, that SSDT is
  refused and 13 load. With every function answering its Header Type and
  bus registers as Linux on the T14 reads them, all 14 load, and 42 methods
  are refused for a function above their region that is not there. That
  reading was taken after Linux enumerated the buses, and Linux may number a
  bridge the firmware left unnumbered: it does not show what the firmware
  leaves at boot, and nothing here does. Owner: the power-off stage, which
  puts the interpreter in the server. **Exit**: on the T14 the server logs
  the load result of each of the 14 tables and a T14 row reads all 14
  there; a table refused for a bridge's answer is brought to the owner with
  that bridge's Header Type and bus registers as the firmware left them,
  and he rules whether a table real firmware ships may be refused for it.
- **The T14's processor objects need `Load`.** Its tables hold eight `Load`
  opcodes and one `LoadTable`, none run while a table loads, and Linux lists eight tables
  loaded that way; the interpreter refuses both as unsupported.
- **A refused evaluation keeps what it stored**, and nothing gives an
  interpreter's 16 MiB back: after one method has filled it, a name's value
  still evaluates, `\_S5`'s package among them, and every method that must
  hold anything new is refused
  (`what_is_held_live_is_bounded_in_sum`, `userland/acpiserver/aml/tests/hostile.rs`).
  Owner: the power-off stage, which decides
  what the server does with an interpreter that is full. **Exit**: a test
  fills the budget through one method, and the server then evaluates a
  method that builds a buffer.
- **The interpreter's 16 MiB is its meter's count, not its heap.** The meter
  counts whatever a table sizes; each namespace node and package element
  carries a constant beside that which it does not count (`object::Meter`,
  `userland/acpiserver/aml/src/object.rs`). With the meter full, the heap an
  interpreter held was 16,776,232 bytes when buffers filled it, 19,549,520
  when package elements naming objects not yet defined did, and 41,091,744
  when field units did. A load refused at the bound also leaves the
  namespace's arena at the capacity it grew to: 24,115,888 bytes held after
  one table naming 204,000 field units was refused. Owner: the power-off
  stage, which gives the server its memory. **Exit**: the server states its
  interpreter's bound in heap bytes, and a host test under a counting
  allocator holds each of those three fills and that refused load to it.

**Stage: power-off through the server** (the orchestrator's placement of "Yes,
one path"). The ACPI server evaluates `\_S5` and powers the machine off.
Blocked on the interpreter's evaluation of `\_S5`. **Exit**: the kernel's `\_S5_` reader, `toyos-acpi/src/dsdt.rs`, and its caller
in `kernel/src/arch/x86_64/power.rs` are deleted, and a test that powers off
through a broken server is red.
