---
status: open
kind: defect
opened: 2026-09-14
---

# A fatal event stands down both bounds, and on a machine that cannot reset itself nothing is left to end the boot

`apic::halt_all_cpus` (`kernel/src/arch/apic.rs:228`) calls
`crate::hardlockup::stand_down()` and `crate::deadline::stand_down()` before it
holds the panel (`kernel/src/arch/apic.rs:235-236`). That is deliberate and the
deadline's own header says so — "a panic in progress, which is not a gap but a
stand-down: `apic::halt_all_cpus` calls `stand_down` before it holds the panel,
so a panic report is never replaced by an expiry"
(`kernel/src/deadline.rs:30-32`). Both of this machine's bounds are gone from
that instant: the 120 s boot deadline polled from the timer entry, and the 60 s
per-CPU hard-lockup NMI.

**What is left is one bound, and it is conditional.** `panic_reboot::arm`
returns `Bound::At(cycles)` only when `acpi::can_reboot()` holds, and
`can_reboot` is `RESET_PORT != 0` — the FADT's reset register and nothing else,
with no fallback by design (`kernel/src/drivers/acpi.rs:277-283`). Where the
table named none, `arm` returns `Bound::Held`
(`kernel/src/panic_reboot.rs:134-145`) and `hold_the_panel` loops
`while bound.is_armed()` (`kernel/src/drivers/panic_console/mod.rs:647-650`),
which never ends. A keypress also retires the bound, which is right for a
machine with somebody in front of it and is exactly wrong for one running
unattended.

**On the T14 that last bound is the one thing already known not to work.**
`issues/hardware/an-armed-tco-has-never-reset-the-t14.md` records that no claim
of this machine resetting itself has ever survived: every metal run so far has
needed a hand on the power button. So a fatal kernel event on this machine
disarms the two bounds that would have ended the boot and then depends on the
one mechanism that has never been observed to fire.

## The evidence, and why it is thin on purpose

Metal run 49 (`lancase`, branch `i219-phy`) was away 303 s with no ping answered
at any point and no readback at all — the boot left the USB device in a state
its next host could not enumerate, so `/dev/sda3` never came back. A healthy
`lancase` boot returns in 60–85 s and a boot the 120 s deadline ends returns in
about 220 s, so 303 s is past the deadline path. **Nothing from the machine
side survived**, which is the shape this file is about rather than a gap in it:
the channels that would have named the fault are the record ring (reaches the
stick only through `logd`) and a sealed `WEDGED` record (`seal_wedge`, reached
from `deadline::expire` and `hardlockup::locked_up`) — and the stand-down is
what guarantees the second one is not written.

This is not
`issues/panic-path/no-console-between-boot-and-terminal.md`, which is about a
machine having no output channel in a window. Here the channel exists and the
mechanism that would use it has been disarmed.

## What would answer it

The stand-down is right in its own terms — an expiry must not overwrite a panic
report — and what is missing is a bound on the *report* itself on a machine with
no operator. Either the panel hold carries a bound that does not rest on the
FADT register, or a boot that reaches `halt_all_cpus` on a machine that cannot
reset says so in the one place that crosses a reset, so the next boot's loader
prints it.

**Exit condition**: a boot that takes a fatal event on the T14 with nobody in
front of it either ends itself and leaves a record naming the fault, or the run
shows which of `Bound::Held` and the retired-by-a-keypress path held it — read
off the machine, not argued from the tree.
