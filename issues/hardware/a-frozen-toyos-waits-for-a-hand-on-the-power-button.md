---
status: open
kind: track
opened: 2026-09-30
---

# A frozen ToyOS waits for a hand on the power button

No shipped image arms a hardware watchdog or the hard-lockup detector: the
kernel arms the chipset's TCO only on the `watchdog` parameter
(`kernel/src/arch/x86_64/watchdog.rs:48-51`), feeds it from the scheduler pass
(`kernel/src/sched/driver.rs:512`), and arms the detector only through
`boot-deadline=` (`kernel/src/deadline.rs:168-183`). The owner's rulings: a
userland `watchdogd` feeds the watchdog on every machine, with no new syscall;
the detector is armed on every boot; nothing in the kernel exists for tests
alone.

- **Refused by right.** `watchdogd` reaches the TCO through a device class of
  its own, as soundd reaches HDA (`kernel/src/syscall/device.rs:55-77`). Any
  `device` holder may mint a claim (`:123-158`), every test job among them
  (`tests/testcases/system.toml:41`), so `sys_device_claim` demands a new
  `Rights::WATCHDOG` for the class, which no `syscap` name grants: only init
  holds it, and mints the claim for the row whose `devices` names the class.
  The bit and the class are an ABI change.
- **The kernel feeds while no claim is held**, so a `watchdogd` that ends
  hands the timer back until `restart` brings it up; a release that outlives
  its syscall (`issues/kernel/deferred-release-outlives-its-syscall.md`) would
  leave nobody feeding, so `watchdogd` waits on it.
- `watchdogd` feeds from a thread under `rt`, until
  `issues/kernel/cpu-time-is-a-band-and-not-a-reservation.md` gives it a
  reservation.
- **A panic's panel holds as it does today** and feeds the watchdog while it
  holds, after a key too (`kernel/src/drivers/panic_console/mod.rs:710-725`).
- **QEMU judges no reset it did not stage.** q35's TCO counts
  `QEMU_CLOCK_VIRTUAL`, which a loaded host advances while it starves a guest,
  and its second expiry does what `-action watchdog=` says (QEMU 11.1.1,
  `hw/acpi/ich9_tco.c:61-70,244`). Every guest but one that stages a reset runs
  with `-action watchdog=none`; that a feeding kernel is not reset is
  `watchdog_fed`'s verdict, on the T14 (`tests/toyos.rs:2034-2038`).
- The T14's PCH TCO is its one reset that needs nothing of the kernel (no BMC,
  no AMT, a battery no switch cuts), and no armed TCO has reset it yet
  (`issues/hardware/an-armed-tco-has-never-reset-the-t14.md`).

**Next: the detector on every boot**, at the half of `boot-deadline=` it takes
today or at `toyos_tco::HARD_LOCKUP_BOUND_MS` where a boot names none, since a
CPU frozen with interrupts off is a hand on the power button while `watchdogd`
on the other CPUs feeds. All 23 boots of a full T14 run were armed, and only
the staged `hardlockup` boot fired.

**Exit**: a boot that names no `boot-deadline=` says `hard lockup:` with
`HARD_LOCKUP_BOUND_MS`, and with the arm back behind `deadline::start` it says
it has no bound.

**Then, on the owner's yes** to
`issues/hardware/whether-a-boot-may-starve-the-t14s-watchdog-is-the-owners.md`:
**the reading.** One T14 boot armed with `watchdog` and `tco-starve`, which
`src/metal.rs`' `FLASHABLE` and `WEDGE_ARMS` must first name, under a deadline
past `tco-starve`'s 5 s and `toyos_tco::BOUND_MS`, which
`toyos_tco::STAGED_BOUND_MS` is not. A TCO reset seals no record, and Ubuntu's
`iTCO_wdt_probe` clears `SECOND_TO_STS` (Linux 6.12,
`drivers/watchdog/iTCO_wdt.c:545-560`), so the reading is the loader's report
pass right after the reset, which reads `TCO2_STS` with the page until loader
stage 8 takes the TCO out of the loader. The fed control is `watchdog_fed`,
once `issues/hardware/watchdog-fed-ends-its-boot-before-the-bound-it-judges.md`
closes; the positive control is q35's `watchdog_resets`
(`tests/common/power.rs:515-536`), read through the pass after its reset.

**Exit**: that pass finds no record on the page, which the deadline behind
the starve would have sealed, and reads `TCO_SECOND_TO_STS` set; the pass after
`deadlinewedge` finds its record and reads the latch clear. Otherwise the T14
exit below waits on the TCO defect.

**Last: `watchdogd` ships**, after loader stage 8
(`issues/boot-media/the-loader-does-only-what-must-precede-the-handover.md`)
has made the kernel's arm the only one. `toyos_tco::PARAM` goes, and with it
`watchdog_quiet`, `loader_watchdog_arms`' control arm and the
`testcases-watchdog` boot: the kernel arms every boot whose chipset has a
`toyos_tco` row, q35 included, and one with none says it is unwatched.

**Exit**: in QEMU, a kernel wedged by `wedge-before-reset` with `watchdogd`
feeding ends in the chipset's reset, and a job holding `device` on an image
with no `watchdogd` is refused the class `PermissionDenied`, which it mints with
the right check taken out of `sys_device_claim`. On the T14 the boot after a
wedge the TCO ended says so, and the boot after a held panic does not.
