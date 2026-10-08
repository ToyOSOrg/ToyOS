---
status: open
kind: defect
opened: 2026-10-08
---

# A firmware call does what its handler chooses, and the kernel bounds only the call

A byte the `acpi` claim's holder stores to the FADT's `SMI_CMD` is a call into
the firmware, which the kernel makes for it
(`call`, `kernel/src/arch/x86_64/acpi_mode.rs`; decided by
`toyos_userbound::firmware::port`). The byte selects a handler that runs in
system management mode, above the kernel, and reads whatever the holder wrote
to firmware's memory before the call, which the mediated access passes both
ways (`issues/the-acpi-claims-holder-reaches-every-port-and-firmware-range-the-kernel-did-not-declare.md`).
So a bug in `/system/bin/acpiserver`, or AML it runs, reaches whatever the
machine's firmware does on any byte with any argument block.

What the kernel bounds:

- **Who**: a process that holds the claim, bound to it.
- **When**: never once the stop has begun.
- **Where**: the boot processor, read beside the `out` (ACPI 6.5 Table 5.9,
  as `kernel/src/arch/x86_64/smi_cmd.rs` quotes it).
- **Which byte**: none the FADT gives a meaning, `ACPI_ENABLE`,
  `ACPI_DISABLE`, `S4BIOS_REQ`, `PSTATE_CNT` and `CST_CNT`, each the kernel's
  own command or nobody's; a field the FADT leaves zero names none. A write
  wider than a byte that reaches the port is refused whole.
- **How often**: eight in any second (`firmware::CALLS`,
  `toyos-userbound/src/firmware.rs`), counted for every holder there has
  been. The ninth is refused to the caller by name and written nowhere.

What it does not:

- **What the handler does.** Nothing reads the argument block, and nothing
  could hold it to a meaning: the handlers are the machine's maker's.
- **How long a call holds the machine.** The write returns when the handler
  does, with the boot processor's interrupts closed and the asker spinning
  under the mediation's lock; a software interrupt to the firmware stops
  every CPU. The kernel counts and times each
  (`toyos_abi::counters::Counter::FirmwareCalls` and `FirmwareNanos`, and a
  line for the first call of each byte) and bounds none.
- **A machine whose FADT names no `SMI_CMD`.** Nothing is declared there, so
  its chipset's command port is a port like any other and the holder writes
  it with no bound at all.

What is measured. Eight a second is this kernel's own number and no
measurement: the model of the T14's AML that was read makes two calls in one
evaluation at the most, and retries a call its handler has not answered once
a millisecond, ten thousand times; eight holds that storm to eight calls a
second and refuses the rest, and whether it refuses a call a healthy machine
needs is unread. No call but the kernel's own enable and disable has been
made on the T14: the server passes no write its AML asks for to the kernel
yet (`userland/acpiserver/src/host.rs`), so nothing there asks for one. On QEMU's q35 the chipset model keeps the byte and its
`SMI_EN` reads 0, so no guest's call interrupts a firmware, and what a guest
reads of one is that it was written, where, and how often.

The server holds no bound of its own on the calls one evaluation makes: that
is the slice's that evaluates the methods which call.

Owned by `issues/toyos-runs-the-machine-in-acpi-mode-and-interprets-its-aml.md`,
beside `issues/the-acpi-servers-holder-drives-the-embedded-controller-unfiltered.md`.

**Exit**: a call is made only for a byte the machine's loaded tables store
to the port, checked by something other than the holder, or the owner rules
the bound above is the one ToyOS keeps; and the rate is held against the
calls a T14 row reads the machine's own AML making, with the time each held
the boot processor.
