---
status: open
kind: track
opened: 2026-09-30
---

# A frozen ToyOS waits for a hand on the power button

No shipped image arms a hardware watchdog or the hard-lockup detector: the
kernel arms the chipset's TCO only on the `watchdog` parameter
(`arch::watchdog::init`), feeds it from the scheduler pass
(`kernel/src/sched/driver.rs:512`), and arms the detector only through
`boot-deadline=` (`kernel/src/deadline.rs:168-183`). The owner's rulings: a
userland `watchdogd` feeds the watchdog on every machine, with no new syscall;
the detector is armed on every boot; nothing ships for tests alone.

- `watchdogd` reaches the TCO through a device class of its own, as soundd
  reaches HDA (`kernel/src/syscall/device.rs:55-77`), and init mints its claim
  for `watchdogd`'s row as it mints blockd's (`system.toml:182-186`). The class
  is an ABI change. A job holding `device` can mint it where no `watchdogd`
  holds it, which
  `issues/isolation/every-job-test-runner-starts-holds-its-whole-system-capability.md`
  fences.
- **The scheduler pass feeds from the kernel's arm until the class is first
  claimed, and never after**, so a `watchdogd` that ends with no successor
  holding the class resets the machine. A successor's claim that meets its
  predecessor's release still in flight
  (`issues/kernel/deferred-release-outlives-its-syscall.md`) is refused, so
  `watchdogd` waits on that defect.
- `watchdogd` feeds from a thread under `rt`, until
  `issues/kernel/cpu-time-is-a-band-and-not-a-reservation.md` gives it a
  reservation.
- **A panic's panel holds as it does today** and feeds the watchdog while it
  holds, after a key too (`panic_console::hold_the_panel`): the one feed the
  kernel keeps once the class is claimed.
- **Every guest runs with `-action watchdog=none`.** q35's TCO counts
  `QEMU_CLOCK_VIRTUAL`, which a loaded host advances while it starves a guest,
  and its second expiry does what `-action watchdog=` says (QEMU 11.1.1,
  `hw/acpi/ich9_tco.c:61-70,244`). A TCO reset, and a feeding kernel that
  `watchdog_fed` finds not reset, are judged on the T14.
- The T14's PCH TCO is its one reset that needs nothing of the kernel (no BMC,
  no AMT, a battery no switch cuts), and no armed TCO has reset it yet
  (`issues/hardware/an-armed-tco-has-never-reset-the-t14.md`).

**Next: the detector on every boot**, at the half of `boot-deadline=` it takes
today or at `toyos_tco::HARD_LOCKUP_BOUND_MS` where a boot names none. The
constant then has a reader, which closes
`issues/build/hard-lockup-bound-ms-is-read-by-nothing-but-its-own-assertion.md`.

**Exit**: a host test holds the bound of a boot that names no `boot-deadline=`
to `HARD_LOCKUP_BOUND_MS`, and putting the arm back behind `deadline::start`
fails to build. `Armed`'s `(0, _)` arm, which nothing reaches, is gone.

**Then, on the owner's yes** to
`issues/hardware/whether-a-boot-may-starve-the-t14s-watchdog-is-the-owners.md`,
and once loader stage 7 has taken Ubuntu, whose `iTCO_wdt_probe` clears
`SECOND_TO_STS` (Linux 6.12, `drivers/watchdog/iTCO_wdt.c:545-560`), out of the
loop: **the reading**, `watchdog_resets`' metal row in
`issues/build/the-guest-suite-runs-only-what-no-cheaper-tier-reaches.md`. One
T14 boot is armed with `watchdog` and `wedge-before-reset`, under a deadline
past its job list and `toyos_tco::BOUND_MS`, which `toyos_tco::STAGED_BOUND_MS`
is not. The fed control is `watchdog_fed`, once
`issues/hardware/watchdog-fed-ends-its-boot-before-the-bound-it-judges.md`
closes.

**Exit**: the next armed kernel's `TCO2_STS` read, in `arch::watchdog`'s
`arm`, says the last boot ended in a TCO reset, and the page still reads
`ARMED` where the deadline would have sealed a record; the armed boot after
that says no TCO reset. That closes
`issues/hardware/tco2-sts-clearing-is-verified-on-qemu-only.md`. Otherwise the
T14 exit below waits on the TCO defect.

**Last: `watchdogd` ships**, after loader stage 8
(`issues/boot-media/the-loader-does-only-what-must-precede-the-handover.md`)
has made the kernel's arm the only one. `toyos_tco::PARAM` goes, and with it
`watchdog_quiet`, `loader_watchdog_arms`' control arm and the
`testcases-watchdog` boot: the kernel arms every boot whose chipset has a
`toyos_tco` row, q35 included, and one with none says it is unwatched.

**Exit**, on the T14: a boot whose `watchdogd` ends with no successor, and one
wedged by `wedge-before-reset` with `watchdogd` feeding, each end in the TCO's
reset, and the boot after each says so; a kernel whose scheduler pass feeds on
once the claim has gone keeps the first up. The boot after a held panic says
no TCO reset.
