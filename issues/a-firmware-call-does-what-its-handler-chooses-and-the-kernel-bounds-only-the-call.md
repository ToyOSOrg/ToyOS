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
  own command or nobody's. ACPI 6.5 Table 5.9 names no other value for the
  port. A zero names none in four of them; `S4BIOS_REQ` names the byte it
  holds, zero too, where the FACS's `S4BIOS_F` is set, and none where it is
  clear (`SmiCmd::named`, `toyos-acpi/src/fadt.rs`, which quotes each). A
  write wider than a byte that reaches the port is refused whole.
- **How often a byte is written to the command port**: eight in any second
  (`firmware::CALLS`, `toyos-userbound/src/firmware.rs`), counted for every
  holder there has been. The ninth is refused to the caller by name and
  written nowhere. That is a bound on writes to `SMI_CMD` and on nothing
  else that raises a firmware interrupt: the chipset's own SMI enable
  register sits at a port no table names and nothing declared, so the holder
  writes it as it writes any port
  (`issues/the-acpi-claims-holder-reaches-every-port-and-firmware-range-the-kernel-did-not-declare.md`).
  Eight a second is no bound on the firmware interrupts a holder can ask
  for.

What it does not:

- **What the handler does.** Nothing reads the argument block, and nothing
  could hold it to a meaning: the handlers are the machine's maker's.
- **How long a call holds the machine.** The write returns when the handler
  does, with the boot processor's interrupts closed in `smi_cmd::answer` and
  the asker, where it is another CPU, spinning under the mediation's lock
  with preemption off; on the T14 a software interrupt to the firmware stops
  every CPU. The kernel counts and times each
  (`toyos_abi::counters::Counter::FirmwareCalls` and `FirmwareNanos`, and a
  line for the first call of each byte) and can end none: system management
  mode takes no interrupt of this kernel's, the NMI among them, which is
  held pending until the handler returns (Intel SDM Vol. 3C, "NMI Handling
  While in SMM").

What a handler that outlasts each of the kernel's bounds meets
(`kernel/src/arch/x86_64/smi_cmd.rs`, decided by `kernel::bootwrite`). The
decision is host-tested; the rest is by reading and staged nowhere, since no
guest's call runs a handler:

- **The kick's bound has no handler in it.** The asker gives the boot
  processor `DEAF_CPU`, 5 s, to say it has the write, which it says before
  the `out`. A handler's time is never read as a boot processor that takes no
  interrupt.
- **A handler that holds the boot processor 5 s is a panic that says so**,
  whichever CPU asked: `smi_cmd: the firmware has held the boot processor in
  its handler for the 0x.. written to SMI_CMD and has not returned` from an
  asker on another CPU that ran meanwhile, at 5 s from the take; and
  `smi_cmd: the firmware held the boot processor ..ns in its handler` from
  the boot processor itself once the write retires, which is the one that
  speaks where the asker was the boot processor. Where the interrupt stopped
  an asker on another CPU too, either may speak: the asker resumes with the
  clock past the span and the round not yet published, and may panic first
  with "has not returned" of a handler that has. Either message names the
  firmware and the byte. So a handler that returns after 5 s still ends the
  machine: the kernel gives up no CPU for that long, as it gives up none to
  a TLB shootdown.
- **The hard-lockup bound, on an image that names a boot deadline**
  (`kernel/src/hardlockup/mod.rs`; half the deadline). Its sample is an NMI,
  delivered to the boot processor when the handler returns and before the
  kernel's own judgement above. It finds `IF` clear in `smi_cmd::answer` and
  no interrupt taken, and where that has lasted its bound it seals a `WEDGED`
  record that names cpu0 and that `pc` and resets the machine, unless an
  asker's panic stood it down first. Whether the counter that raises the
  sample counts in system management mode on the T14 is unread.
- **The boot deadline, on such an image** (`kernel/src/deadline.rs`), is
  polled from a timer entry, and none is taken while every CPU is stopped:
  the first tick after a handler that outlasted it seals `the boot deadline
  expired`, where nothing above took the seal.
- **A handler that never returns.** Where its interrupt stopped every CPU,
  no instruction of this kernel runs again and it says nothing: the machine
  is the firmware's, and a hand on the power button ends it. Where it
  stopped the boot processor alone, an asker on another CPU panics at 5 s
  with the words above. Asked on the boot processor itself, nothing waits on
  the write, and the next wait on cpu0 speaks: a TLB shootdown's `DEAF_CPU`
  panic, or the boot deadline on an image that names one. A machine on which
  neither comes is not ended by this kernel.
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
the boot processor; and the span is ruled. The slice that passes the first
write its AML asks for to the kernel reads on the T14 the time each call its
AML makes holds cpu0, and the owner rules, against those readings, whether a
handler that returns after `DEAF_CPU` ends the machine. Where the interrupt
stops every CPU the kernel such a handler returns to is whole, so the panic
there is this kernel's choice. That slice does not land without the ruling.
