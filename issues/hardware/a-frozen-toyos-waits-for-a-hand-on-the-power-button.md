---
status: open
kind: track
opened: 2026-09-30
---

# A frozen ToyOS waits for a hand on the power button

No shipped image arms a hardware watchdog. The kernel arms the chipset's TCO
only on the `watchdog` boot parameter
(`kernel/src/arch/x86_64/watchdog.rs:48-51`), which no shipped image carries,
and feeds it from the scheduler pass (`kernel/src/sched/driver.rs:512`), which
proves only that some CPU still reaches one; and no armed TCO has yet reset the
T14 (`issues/hardware/an-armed-tco-has-never-reset-the-t14.md`). The owner's
ruling: the watchdog is a production feature, shipped to every machine.
`watchdogd`, a userland service, claims the machine's hardware watchdog and
feeds it while the system is healthy; if ToyOS freezes or its userland stops
being scheduled, the hardware resets the machine.

- **No syscall.** The watchdog becomes a device class of its own, and
  `watchdogd` reaches its registers through that class's allow-list in
  `sys_device_reg` (`kernel/src/syscall/device.rs:55-77`), as soundd reaches
  HDA's.
- **Only `watchdogd` holds it.** Any holder of `device` may mint a claim
  (`kernel/src/syscall/device.rs:123-141`), test-runner and every job it starts
  among them (`tests/testcases/system.toml:41`), and the first mint ends the
  kernel's feed: so the kernel refuses the class to every other holder, which
  a build refusing it to other rows does not do.
- **Fed from the real-time band.** `watchdogd` feeds from a thread of its own
  under `rt`; `issues/kernel/cpu-time-is-a-band-and-not-a-reservation.md`
  replaces that precedence with a reservation, which `watchdogd` then holds.
- **Nothing in the kernel is added for tests only** (owner ruling): a test
  harness uses this watchdog as it ships.
- The T14 has no BMC or AMT (its i5-1135G7 has no vPro) and a battery no power
  switch cuts, so its PCH's TCO is its one reset that needs nothing of the
  kernel.
- **A panic's panel holds as it does today**, for `toyos_tco::PANIC_BOUND_MS`
  or for good once a key is pressed: its hold feeds the watchdog for as long
  as it holds, the hold after a key included, which halts its CPU today
  (`kernel/src/drivers/panic_console/mod.rs:710-725`). A panic that never
  reaches the panel stops every feed, and the watchdog resets the machine.

## Stages

1. **The measurement, run on the T14 only on the owner's go-ahead.** Two boots.
   The first is armed with `watchdog` and `tco-starve`, which `src/metal.rs`'
   `FLASHABLE` and `WEDGE_ARMS` must first name, and a job holds it past the
   bound, since the shutdown disarms the timer
   (`kernel/src/syscall/machine.rs:88`); its `boot-deadline=` ends it if the
   TCO does not. A TCO reset seals no record, so the reading is the second, the
   next armed boot: its loader's `TCO2_STS` and its kernel's
   `watchdog: the last boot ended in a TCO reset`
   (`kernel/src/arch/x86_64/watchdog.rs:92-99`), which the report pass in
   between does not print. That line is also the reading
   `issues/hardware/tco2-sts-clearing-is-verified-on-qemu-only.md` waits on.
   The fed control is `watchdog_fed`, once
   `issues/hardware/watchdog-fed-ends-its-boot-before-the-bound-it-judges.md`
   holds its boot past the bound; the positive control is q35's starved guest,
   `watchdog_resets` (`tests/common/power.rs:515-536,694-701`).

   **Exit**: the armed boot after the starved one reads `TCO_SECOND_TO_STS`
   set. If it reads it clear, the track stops and goes back to the owner with
   both boots' readbacks.

2. **`watchdogd` ships**, after the loader track's stage 8
   (`issues/boot-media/the-loader-does-only-what-must-precede-the-handover.md`)
   has made the kernel's arm the only one. `toyos_tco::PARAM` goes: the kernel
   arms on every boot whose chipset `toyos_tco` has a row, q35 included, and
   feeds from the scheduler pass until the claim is minted and never after.
   With the parameter go `watchdog_quiet`, `loader_watchdog_arms`' control arm
   and the `testcases-watchdog` boot, since every boot is then armed. A machine
   with no row refuses the claim as absent, and the boot says it is unwatched.

   **Exit**: in QEMU, a kernel wedged by `wedge-before-reset` with `watchdogd`
   feeding ends in the chipset's reset; a job holding `device` is refused the
   class; and a whole QEMU run resets no guest it did not stage. On the T14,
   the boot after a wedged one reads the latch set, and the boot after a panic
   whose panel held reads it clear.
