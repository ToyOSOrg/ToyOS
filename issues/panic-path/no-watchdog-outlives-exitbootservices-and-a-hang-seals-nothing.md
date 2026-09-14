---
status: open
kind: defect
opened: 2026-09-14
---

# Past `ExitBootServices` nothing times a hang but the FADT reset register, and `halt_all_cpus` disarms the one thing that would say so

**The firmware watchdog does not survive the handoff.** `bootloader/src/main.rs`
arms `set_watchdog_timer` before anything else (`:798-799`) and says why at
`:864`: *"Firmware watchdog: `{FIRMWARE_WATCHDOG_SECS}` s, until
`ExitBootServices` disables it"* — that timer only guards a loader that never
calls `ExitBootServices`. Once the kernel is running, the only bounds left are
the ones it arms itself — the boot-progress deadline `deadline::stand_down`
disarms and the per-CPU hard-lockup NMI `hardlockup::stand_down` disarms
(`kernel/src/deadline.rs`, `kernel/src/hardlockup/mod.rs`) — and optionally the
chipset's own TCO watchdog if a boot's command line arms it
(`bootloader/src/watchdog.rs`) — which `issues/hardware/an-armed-tco-has-never-reset-the-t14.md`
records as never having reset this machine at any bound reached so far.

**And a fatal kernel event disarms both of the kernel's own bounds before it
holds the panel.** `apic::halt_all_cpus` (`kernel/src/arch/apic.rs:228`) opens
with `crate::hardlockup::stand_down()` and `crate::deadline::stand_down()`
(`:235-236`), then arms one conditional bound —
`crate::panic_reboot::arm(true)` (`:244`) — and hands off to
`panic_console::page_forever` (`:262`), which for every "cannot paint"
branch and for a successful paint alike ends in `hold_the_panel`
(`kernel/src/drivers/panic_console/mod.rs:610-620,647`): `while
bound.is_armed()` (`:649`), which never exits on its own.

`panic_reboot::arm` (`kernel/src/panic_reboot.rs:110`) returns `Bound::At`
(`:132`) only where both a clock is available *and* `acpi::can_reboot()` holds
— `RESET_PORT != 0` (`kernel/src/drivers/acpi.rs:277-278`), the FADT's reset
register and nothing else, no fallback by design — and `Bound::Held`
otherwise: once for no reset register (`:144`) and once for no usable clock
(`:155`). Either is the branch `hold_the_panel` spins on forever. A keypress
also retires the bound (the panel is designed for a machine with someone in
front of it), which is exactly wrong for an unattended bench run.

**The one channel that would say a hang happened is disarmed by the same
stand-down.** `seal_wedge` (`kernel/src/drivers/panic_console/mod.rs:597`) has
exactly two callers in the whole kernel — `kernel/src/deadline.rs:214` and
`kernel/src/hardlockup/mod.rs:319` — and both are the `stand_down` calls
`halt_all_cpus` makes before it ever reaches `hold_the_panel`. So a fatal event
that reaches `halt_all_cpus` writes its panic report to the panel and to
serial (where one exists), but never reaches the black-box path that a
`WEDGED` seal would leave for the *next* boot's loader to print — and on a
machine that never resets, there is no next boot to print it to anyway.

## What this means on the T14

`RESET_PORT` is whatever the FADT named — unverified as arming on this
machine independent of the TCO question — and the TCO path is separately
recorded as never having fired. So a kernel panic on the T14 disarms the two
bounds that could have ended the boot, arms a third whose one working
precondition is unproven on this hardware, and forecloses the one record that
would have said which of the two outcomes happened. The observable result —
silence past whatever deadline the harness gives it, with neither the panel
nor the log partition readable afterward — is indistinguishable at the bench
from a machine that simply never got that far. This has not yet been read off
a boot: `issues/build/a-hung-boots-log-partition-is-wiped-by-the-next-runs-flash.md`
is why no hung boot's own `/log` or panel state has been captured yet.

## What would answer it

Either the panel hold gets a bound that does not rest on the FADT register (so
an unattended machine ends the boot on its own within a stated time), or a boot
that reaches `halt_all_cpus` on a machine that cannot reset says so somewhere
that survives to the next boot regardless of whether the panel is ever
retired. Until one of those lands, a fatal kernel event past `ExitBootServices`
on this machine is timed by nothing this tree has verified to fire.
