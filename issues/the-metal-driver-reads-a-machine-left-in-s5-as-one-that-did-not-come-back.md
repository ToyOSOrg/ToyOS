---
status: open
kind: tooling
opened: 2026-10-04
---

# The metal driver reads a machine left in S5 as one that did not come back

`toyos-metal`'s `run` (`src/metal.rs`) reboots the T14 into the flashed image
and `ride_the_reboot` waits up to `return_secs()`, 420 s, for Ubuntu's ssh to
answer again. A boot that ends in a correct power-off leaves the machine in S5,
and nothing in the loop can power it on: it answers again only if a hand
powers it on within those 420 s. Two rows end that way by design,
`acpi_power_off` and `acpi_power_button_pressed` (`tests/toyos.rs`).

Past the bound the driver refuses with `the machine did not come back within
420 s, which is longer than every watchdog a boot runs under plus the time
coming back costs`, which is false of a machine in S5, and returns before
`read_log`: `clear_readback` has already emptied the readback directory, so
the boot leaves no `verdict.txt`, `boot.txt`, `loader.log` or `kernel.log`,
and the harness cannot judge the row. The stick still holds the boot's log
until the next flash
(`issues/a-hung-boots-log-partition-is-wiped-by-the-next-runs-flash.md`).

At `ee6aadecb` the attended `testcases-press` boot did this: its log, read off
a copy of the stick's partition taken before the next run's flash, ends at
50.180 s with the server's power-button line and the supervisor's
`(Shutdown)`, the owner found the machine off and powered it on by hand, and
the driver had refused with the line above, exiting 1. Its loader and kernel logs,
copied out of that partition into a readback directory, are judged by
`cargo test --test toyos-build -- --metal --metal-readback <dir>
acpi_power_button_pressed` as `FAIL testcases-press: …/verdict.txt: No such
file or directory (os error 2) — no readback for testcases-press`, EXIT=1:
`back_secs` and `stick_secs` were never measured and the loop wrote no
verdict, and nothing short of forging those files makes the boot judgeable.

Owned by `issues/the-t14-reboots-through-ubuntu-for-every-test.md`, whose
plan names every reset in a run by what forces it.

**Exit**: a T14 boot that powers off and is powered on only after
`return_secs()` yields a readback the harness judges on its log.
