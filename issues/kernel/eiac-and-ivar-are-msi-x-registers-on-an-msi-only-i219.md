---
status: open
kind: finding
opened: 2026-09-14
---

# `toyos-i219` cites the 82574 throughout, and writes two MSI-X-only registers to a part measured to be MSI

`toyos-i219/src/regs.rs:1-8` states the file's whole citation policy: "Every
offset and every bit below is cited to the *Intel 82574 GbE Controller Family
Datasheet* … Nothing here has behaviour, and a number that is not in the
datasheet does not belong in this file." `toyos-i219/src/lib.rs:4-9` says why:
the driver runs against the T14's `8086:15fc` I219, whose register file is
taken to be the 82574's — "the datasheet this file cites throughout is the
82574's rather than the I219's own, which describes the part and not the
register set."

Two of those cited registers are conditional on a mode the datasheet names by
name. `regs::EIAC` (`0x000DC`, `regs.rs:27`) and `regs::IVAR` (`0x000E4`,
`regs.rs:30`, doc: "Interrupt Vector Allocation (§10.2.4.9, `0x000E4`), which
'is only valid in MSI-X mode'") are both written unconditionally in `open`
(`lib.rs:399`): `EIAC = 0` at `lib.rs:477`, then `IVAR` written and read back
at `lib.rs:502-503`, with `accepted()` (`lib.rs:542`) refusing
`Refusal::NotAccepted` if the readback does not carry the bits written. Any
`Err` out of `open` reaches `userland/netd/src/main.rs`'s `Card::intel`
(`:87-91`), which turns it into a panic: `Card::undrivable` at `:83-84`.

**The T14's `00:1f.6` has been measured to be an MSI part, not MSI-X.** That
measurement is not yet in this tree's copy of
`issues/hardware/the-t14-answers-only-through-a-usb-stick.md` — it was made and
recorded on a sibling, unmerged branch (`git show 0a5717f5:issues/hardware/the-t14-answers-only-through-a-usb-stick.md`):
"`/proc/interrupts` names its interrupt `IR-PCI-MSI-0000:00:1f.6` and
`msi_irqs/162` reads `mode=msi`." Read together with `regs.rs`'s own citation,
`IVAR` is a register this driver writes and requires a readback from on a part
the datasheet's own §10.2.4.9 says the register is not defined for outside
MSI-X mode.

**And the write has been observed to go through anyway.** On that same branch,
bench runs 42 and 51 (`lancase`/`lancase-placement`, `t14-run42`, `t14-run51`)
carry no `exit: netd` record at all — and a `Refusal` out of `open` panics
`netd` by name, so its absence means `accepted(regs::IVAR, …)` returned `Ok`
there: the T14 took the write and echoed the bits back, on a register its own
family's datasheet does not define for the mode the function is in. That the
readback matched is not evidence the register does what §10.2.4.9 documents;
it is evidence only that *some* word landed where the driver looked for it.

## What would answer it

Whether `0x000DC` and `0x000E4` do anything defined on an I219 outside MSI-X
mode is not in the 82574 datasheet this file cites, and the I219's own
datasheet — which `lib.rs:8` already says this driver does not use — is the
document that would say. Until that is read, this file's own citation policy
("a number that is not in the datasheet does not belong in this file") is
already violated by two rows whose behaviour outside MSI-X mode is
undocumented by the datasheet named, and the readback guard the driver relies
on to catch a wrong register file has been measured not to catch this one.
