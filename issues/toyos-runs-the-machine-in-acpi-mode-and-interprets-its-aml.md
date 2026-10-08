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
  boot held open for it. Superseded by the two rulings on tests that follow.

**Ruled** (owner, 2026-10-05): "A test that requires manual steps from me is
forbidden." **Ruled** (owner, 2026-10-05, on what a T14 test may need): "No
automated test is allowed that requires physical buttons to be pressed or
anything we cant do now with the t14. I can test it on demand but no ci there
not always someone available physically". No T14 row of this track needs a hand: not on the
button, and not to power the machine on again.

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
fixed event that needs no AML, and takes the EC's events. **Exit**: QEMU's
`acpi_power_button` reads that a press the server serves stops the machine
cleanly, through ToyOS's own power-off path (`SYS_SHUTDOWN`), with the press
and that stop in the boot's log. On the T14, `counters` reads
`MSR_SMI_COUNT`, through the general counters and not by a check of its own,
flat on every CPU over the interval the firmware issue's exit defines, and the
machine still in ACPI mode; `acpi_server_events` reads each EC query number
once with its count; and `acpi_server_death` kills the server and reads
`SCI_EN` clear in `PM1_CNT` afterwards, the kernel having written
`ACPI_DISABLE` to `SMI_CMD`.

Two things stage 1 lands are read by no T14 row, and neither is in its exit.
The T14's own button, and that every press of it stops the machine, is
`issues/the-t14s-power-button-event-came-up-to-17-s-after-ec-query-0x28.md`'s
(owner, 2026-10-05, "Land it, record the gap": "The AML stage closes it").
The T14's power-off after the kernel's own `ACPI_ENABLE`, which that ruling
lists among what stage 1 lands, was last read at `8d7004d3b` on a boot the
owner powered on again and is read on q35 only in a machine its firmware put
in ACPI mode; that no T14 row reads it is
`issues/no-t14-row-reads-the-power-off-after-the-kernels-own-acpi-enable.md`'s.
Moving it out of this exit is the orchestrator's placement, not the owner's.

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

**Stage: the interpreter** (the orchestrator's placement of "The AML stage
closes it"). ToyOS's own AML interpreter, written from the specification, run
by the ACPI server, the battery first: `userland/acpiserver/aml`, pure and
host-tested, beside the server, which does not link it yet. It owns
`issues/the-t14s-power-button-event-came-up-to-17-s-after-ec-query-0x28.md`.
**Exit**: a host test loads QEMU 11.1.1's DSDT
(`toyos-acpi/fixtures/qemu-11.1.1/dsdt.bin`) and evaluates `\_S5` to the
`SLP_TYPa` its boot logged, 0; and the T14's DSDT and SSDTs, read by a
check run outside the tree, load and evaluate `\_S5`, its pull request
recording the result; and on the T14 the server evaluates the method of each
embedded-controller query the tables define, a T14 row reads the battery's
state as the interpreter evaluated it beside Linux's reading of the same
machine, and the press issue is closed by its own exit.

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
  The same row reads that no method the server evaluated was refused for a
  bridge's answer, and one that was goes to the owner with the table
  refusal.
- **The T14's processor objects need `Load`.** Its tables hold eight `Load`
  opcodes and one `LoadTable`, none run while a table loads, and Linux lists eight tables
  loaded that way; the interpreter refuses both as unsupported.
- **A refused evaluation keeps what it stored**, and nothing gives an
  interpreter's 16 MiB back: after one method has filled it, an Integer
  still evaluates, a field that fits one among them, and every method that
  must hold anything new is refused; so is a String, Buffer or Package
  handed to the caller, `\_S5`'s among them, where the bound has no room
  for the caller's copy while it is built
  (`what_is_held_live_is_bounded_in_sum`, `userland/acpiserver/aml/tests/hostile.rs`;
  `a_full_interpreter_reads_a_field_that_fits_an_integer`, `userland/acpiserver/aml/tests/heap.rs`).
  Owner: the power-off stage, which decides
  what the server does with an interpreter that is full. **Exit**: a test
  fills the budget through one method, and the server then evaluates a
  method that builds a buffer.
- **The interpreter's 16 MiB bounds the heap it holds from one call to the
  next, not what one call holds while it runs.** The meter counts what an
  interpreter's allocations ask for (`object::Meter`,
  `userland/acpiserver/aml/src/object.rs`), and under a counting allocator
  an interpreter filled until it refuses holds at most `MAX_LIVE`, whatever
  fills it; a refused load leaves it holding the bytes it held before;
  ToString, Mid and Concatenate nested in themselves, and fields read
  through each other, hold one level's bytes past what the meter counts and
  not a level's each; and the value an evaluation hands its caller is held
  to the bound while it is built (`userland/acpiserver/aml/tests/heap.rs`).
  That allocator counts every realloc as one that moves. While a load or an
  evaluation runs it still holds more, uncounted, and nothing measures the
  sum. Each part is bounded by the depth or the step bound, by reading and
  not by a run: its frames, 256 at most; a name read from the table for
  each, 255 segments at most; 9 bytes for every Mutex acquired and not
  released, an Acquire two steps at the least; 16 bytes for each of at
  most 256 devices above a PCI_Config region while its bridges are asked;
  one operator's string or buffer before the meter holds it, Concatenate's
  the most at its two operands' copies and their sum; a table's bytes while
  they are read in; the arena's old slots while a doubling moves it; and
  the segments and the text of one node's path, as deep as the meter
  admits a node, for Notify, a reference handed back and the name a
  refusal carries. A refusal's text is the caller's and outside the meter.
  The meter also holds a namespace node above its cost, at a whole
  map leaf of 104 bytes for its entry in its parent and the arena at its
  doubled capacity: an interpreter filled with field units refuses with
  11,111,460 bytes of heap held, and one filled with devices of one child
  with 8,900,612. The real machine's tables have not been loaded since the
  meter came to count a node's slot, entry and record, a wide field's read
  came to hold its buffer and the arena's move to cost steps: that reading
  is owed before the server links the crate, and with it the slots the
  arena has free after the last table, since a method that defines more
  names than that moves the arena each time it runs, at a step for every
  64 bytes of it. Owner: the power-off stage, which gives the server its
  memory. **Exit**: the server states its interpreter's bound in heap
  bytes, and a host test under a counting allocator holds the most a load
  and an evaluation hold while they run to it; and the real machine's
  tables load under the meter as it stands, the arena's free slots after
  the last read with them.

The press issue's measurement of 2026-10-07 found the three presses it lost
changing nothing its scout read, with the button's event enabled and no SMI
taken, and its hypothesis is that the controller wants the firmware's
initialisation run first. The slice that puts `_REG`, the `_STA` and `_INI`
walk and the query methods in the server on the T14 is the one that issue's
one-press test waits on. Its reading of 2026-10-08 found bit 0 of the
controller's memory at offset 0x05 set under stage 1 and clear under Linux,
and a dry run of the firmware's `_INI` for the controller clears that bit, so
that slice's first reading on the T14 is the same scout's of that bit, clear
after the initialisation has run, before the press.

**Stage: power-off through the server** (the orchestrator's placement of "Yes,
one path"). The ACPI server evaluates `\_S5` and powers the machine off.
Blocked on the interpreter's evaluation of `\_S5`. **Exit**: the kernel's `\_S5_` reader, `toyos-acpi/src/dsdt.rs`, and its caller
in `kernel/src/arch/x86_64/power.rs` are deleted, and a test that powers off
through a broken server is red.
