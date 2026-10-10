---
status: open
kind: track
opened: 2026-10-03
---

# ToyOS runs the machine in ACPI mode and interprets its AML

The T14's firmware hands the machine over in legacy mode, which interrupts
every CPU every 2.2 s
(`issues/the-t14s-firmware-interrupts-every-cpu-every-2-2-s-under-toyos.md`).

The ACPI server loads a machine's DSDT and SSDTs, evaluates `\_S5` and hands
the kernel its sleep type, which the kernel powers the machine off with and
has from nowhere else; it evaluates nothing else of their AML yet. What else
ToyOS takes from them it takes another way: the loader asks UEFI for the root
bridges' windows `_CRS` would name (`bootloader/src/rootbridge.rs`). Nothing
reads `_CST`, which names a CPU's C-states.

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
and holds the count to what ToyOS asked for over the interval the firmware
issue's exit defines, every CPU's delta equal to the boot processor's
`firmware_calls` delta, and the machine still in ACPI mode; `acpi_server_events` reads each EC query number
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
host-tested, beside the server, which loads the tables with it
(`userland/acpiserver/src/aml.rs`). It owns
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
  leaves at boot, and nothing here does. Owner: this stage. **Exit**: on the T14 the server logs
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
  Owner: this stage, which decides
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
  with 8,900,612. The real machine's tables, loaded outside the tree under
  the meter as it stands, with the one word of memory answered and every
  function answering as Linux reads it: all 14 load in 39,419 steps, 32,946 of them the
  DSDT's, and leave the meter at 2,412,886 bytes over 1,819,038 of heap by
  a counting allocator, in 6,891 nodes. Nothing reads the arena's
  capacity: by its doubling it is 8,192 slots, 1,301 of them free, and a
  method that defines more names than are free moves the arena each time
  it runs, at a step for every 64 bytes of it. Owner: this stage,
  which gives the server its memory. **Exit**: the server states its
  interpreter's bound in heap bytes, and a host test under a counting
  allocator holds the most a load and an evaluation hold while they run to
  it; and the arena's capacity after the real machine's last table is
  read, not derived.
- **The T14's tables are not in name order.** The interpreter's walk of
  the namespace (`Interpreter::walk`, `userland/acpiserver/aml/src/lib.rs`)
  reads siblings as their tables declared them. Of the 267 objects of the
  T14's tables that hold others, 217 declare them against the order their
  names sort in. What its initialisation needs of the order was read only
  so far: the tables define 45 `_INI`, all methods; a dry run that answered
  every read zero but one word of memory and each function's header found
  two of them writing anything, the PCI root bridge's and the embedded
  controller's; and that controller's device is below that bridge, so
  under those answers the two run parent before child whichever order
  siblings take. What the 45 do under the machine's own answers, and
  whether any hangs on a sibling's, nothing has read.
- **An evaluation may ask to wait ten seconds, and no caller names another
  limit.** The limit counts the time asked of Sleep, Stall and Wait, a
  load's too (`MAX_WAIT_US`, `userland/acpiserver/aml/src/lib.rs`), and
  `Interpreter::usage` says what a call asked, the request that was refused
  included. The T14's 14 tables asked for none while they loaded, and no
  method of them has been read refused at it. Owner: this stage. **Exit**:
  the slice that carries a reading of a method refused at ten seconds, on
  the T14 and by `usage`, brings the limit its caller names with that
  reading; until one does, ten seconds stands and nothing names more.

What the server's load of the tables, on the T14 and through the kernel's
mediated access, leaves open:

- **The controller's `_REG(3, 1)` runs on a namespace no `_INI` has run in.**
  The battery's read (`userland/acpiserver/src/battery.rs`) tells each
  controller device its space is there and then reads the battery, and runs
  no `_INI`: the T14's controller `_INI` calls the firmware through
  `SMI_CMD`, which the host does not write. The scouts' dry run read that
  the T14's `_REG` takes another branch before its `_INI` than after it,
  reading and writing controller offset 0x03 where after it touches the
  controller not at all; Linux runs it after. Owner: this stage. **Exit**:
  the slice that runs the `_STA` and `_INI` walk runs it before `_REG`.

- **A take of the Global Lock that the AML sets no bound on ends the server
  where the firmware holds the lock past 1 s** (`RELEASE`,
  `userland/acpiserver/src/host.rs`). The bound is this server's guess, not
  a measurement. The firmware is meant to hold the lock for the run of one
  SMI handler, and no hold has been seen. The T14's battery read took the
  lock 3 times, and the firmware held it for none of them. Linux's
  `ff_gbl_lock` counter there read 0 at 2 minutes of uptime, and a reading
  after a longer session is still owed. A Lock field and an `Acquire(\_GL,
  0xFFFF)` take with no bound. An Acquire with a TimeoutValue waits that
  long, and the time is charged to the evaluation's 10 s. Owner: this
  stage. **Exit**: a hold measured on the T14, either by a contended take
  in the server's own count or by `ff_gbl_lock` after a long Linux session.
  That measurement sets the bound, or replaces it with no bound if the
  firmware never holds the lock.

- **A press during the load waits for it, and a power-off asked during it is
  refused.** The server arms the power button
  and then loads the tables before it serves an SCI, so a press in that time
  latches and is served when the load ends: 77 ms on the T14, measured once,
  and bounded only by what the interpreter lets each table sleep, 10 s. The
  kernel has `\_S5`'s sleep type only once the load has handed it over, and
  until then refuses `SYS_SHUTDOWN` by name, stopping nothing
  (`kernel/src/arch/x86_64/power.rs`); the asker is told and may ask again.
  Owner: this stage. **Exit**, of the press's wait: the slice that keeps the
  namespace serves the SCI while a table loads. The refusal has no exit and
  stays: `\_S5` is evaluated in a namespace the load has built, so no
  power-off is made before the load ends, and what bounds the refusal is the
  load's time. That time is held on the T14 once the `acpi_tables_loaded` row
  reads it under a bound the owner names.
- **A machine with no holder of the `acpi` claim has no power-off.** The
  kernel reads no AML, so a machine whose claim it refuses, one in legacy
  mode with no ECDT or with a control-method power button, has nobody to
  evaluate `\_S5`: `SYS_SHUTDOWN` is refused there, where the kernel's own
  scan of the DSDT powered such a machine off before. Neither the T14 nor q35
  is one: the T14 has an ECDT and a fixed button, and OVMF hands q35 over in
  ACPI mode. Owner: this stage. **Exit**: the ECDT stopgap is deleted, so a
  machine is claimed for what its DSDT names; what a machine with a
  control-method button does for a power-off is ruled with that button's
  device.

  Two more machines have no power-off, and that is the ruled state and no
  weakness with an exit. A boot whose config starts no server: "Power-off
  always goes through the ACPI server" is the owner's "Yes, one path". A
  machine whose DSDT the server refuses: the owner's "Go on, say it loudly"
  (2026-10-07) was given for a refused table, and the option it chose, as
  the orchestrator's record of the session holds it, logs a refused SSDT and
  carries on, and after a refused DSDT still serves the button and offers no
  power-off. That the server says the same, once and at error severity, of a
  machine whose tables it could not read at all or whose `\_S5` is no
  package of two integers or names a value the register does not hold, and
  that a press on any of them is said and dropped, is the slice's design and
  not his ruling.
- **The claim's holder chooses the sleep type a power-off enters.** The
  kernel writes to `PM1a_CNT` the `SLP_TYPa` the holder supplied
  (`acpi_mode::s5`), and has nothing of its own to hold it against. So a
  holder that is wrong or hostile decides which of the field's eight values a
  power-off asked by a holder of `POWER` writes: S1, S3 or S4 where the
  chipset maps them, or a value it maps to nothing, which is the kernel's
  `S5 did not take` panic two seconds after the write. Before, only the
  firmware's bytes chose. The holder cannot cause the write, set `SLP_EN` or
  reach a bit outside 12:10 (`toyos_userbound::firmware::SleepType`), and
  supplies one value under a claim. What it supplied outlives it, so a
  machine whose server died still powers off: the owner's "Keep it"
  (2026-10-08), asked whether the supplied value survives the server dying
  or is withdrawn with its claim. Owner: this
  stage. **Exit**: it stays while "one path" stands, which leaves the kernel
  no second reading; it goes when the owner rules one in, and a guest test
  then supplies a sleep type that is not the machine's and reads it refused.

What the firmware call, which the kernel makes for the server where its AML
stores a byte to `SMI_CMD`, leaves open. The server's AML makes none yet: its
host writes only the embedded controller's space, through the controller's
own ports, and denies every other write AML asks for, passing none to the
kernel (`userland/acpiserver/src/host.rs`).

- **What a call does there is the firmware's**, and the kernel bounds who,
  when, where, which byte and how often:
  `issues/a-firmware-call-does-what-its-handler-chooses-and-the-kernel-bounds-only-the-call.md`.
- **Eight calls in any second is no measurement**
  (`toyos_userbound::firmware::CALLS`), and it bounds a count of calls, not
  the time they hold the machine: the T14's enable held the boot processor
  2.0 to 2.1 ms on three boots and stopped every CPU, so eight a second is
  about 16 ms of the whole machine in every second if a call costs what the
  enable does, and no call's cost has been read. Owner: this stage.
  **Exit**: the slice that evaluates the methods which call brings the T14's
  count of them and the time each held the boot processor, from the
  `counters` row's `firmware_calls` and `firmware_nanos` and the kernel's
  line for the first call of each byte; it reads what eight a second does to
  the audio and latency rows on the T14, a timing verdict coming only from
  there; and the owner rules the number against them.
- **The `counters` row holds every CPU's SMI count to the commands the
  kernel wrote to `SMI_CMD`** between its first read and its last, and to
  nothing else, where it held the count flat; with no call made the two are
  one judgement. It rests on one reading, that the enable moved every CPU's
  count by one: a call that moves a CPU's count by none or by two reds the
  row, and is a reading for the owner and no flake. That equality is the
  exit of
  `issues/the-t14s-firmware-interrupts-every-cpu-every-2-2-s-under-toyos.md`,
  which names the two things it rests on. Before the row is read over a span
  that holds a call, the slice that evaluates the methods which call takes
  the row's two reads where no call is in flight, or bounds each CPU's
  difference by the calls in flight; and reads every CPU's SMI count either
  side of one real call, which only the enable has been.
- **No guest's call interrupts a firmware, and no guest writes the enable.**
  q35's chipset keeps the byte, with `SMI_EN` reading 0, and its firmware
  hands the machine over in ACPI mode. So `acpi_mediated_access` reads that
  a call was written, on which CPU, how often and counted, and that the
  kernel's own commands were not; the time a handler takes, and the SMI
  count either side of one, are read on the T14 alone, and there only for
  the enable and the disable.
- **`acpi_mediated_access` needs one of three calls to be asked off the boot
  processor**, each from a thread that read itself there first. The tree
  has no affinity (`issues/no-test-can-hold-a-thread-on-a-named-cpu.md`), so
  a thread preempted between that read and the kernel's lock may be taken by
  the boot processor, the window
  `issues/no-t14-row-arranges-an-acpi-disable-asked-off-the-boot-processor.md`
  describes. A boot where all three fall in it reds as `no firmware call was
  asked from another CPU`, over three kernel lines reading `asked from cpu0`
  beside the probe's own line naming three non-zero x2APIC ids: that is the
  window and no defect of the write, and it is answered by the pin, never by
  a second run. None has been read.
- **`acpi_mediated_access`'s storm is refused only if nine calls fit in one
  second of the guest's clock.** The probe asks one after another and needs
  the ninth refused `CommandRate`. A guest whose host gives it less than
  nine calls' worth of time in a second of its own clock is refused none,
  and after ten thousand calls reds as `firmware calls in a row were made,
  and none refused`: a dependence on rate in a guest test, the host's load
  and no defect of the bound, which `toyos-userbound`'s host test holds on a
  clock of its own. None has been read.

The press issue's measurement of 2026-10-07 found the three presses it lost
changing nothing its scout read, with the button's event enabled and no SMI
taken, and its hypothesis is that the controller wants the firmware's
initialisation run first. The slice that puts `_REG`, the `_STA` and `_INI`
walk and the query methods in the server on the T14 is the one that issue's
one-press test waits on. Its reading of 2026-10-08 found bit 0 of the
controller's memory at offset 0x05 set under stage 1 and clear under Linux,
and a dry run of the firmware's `_INI` for the controller clears that bit in
its two variants that answer all ones, the six that answer zero showing no
bit, so that slice's first reading on the T14 is the same scout's of that bit, clear
after the initialisation has run, before the press.
