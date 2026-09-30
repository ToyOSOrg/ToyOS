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
proves only that some CPU still reaches one. The owner's ruling: the watchdog
is a production feature, shipped to every machine. `watchdogd`, a userland
service, claims the machine's hardware watchdog and feeds it while the system
is healthy; if ToyOS freezes or its userland stops being scheduled, the
hardware resets the machine.

- **No syscall.** The watchdog becomes a device class of its own, and
  `watchdogd` reaches its registers through that class's allow-list in
  `sys_device_reg` (`kernel/src/syscall/device.rs:55-77`), as soundd reaches
  HDA's. The build grants the class to `watchdogd`'s row alone, as it does
  `swap`.
- **Fed from the real-time band.** `watchdogd` feeds from a thread of its own
  in the real-time band (`syscap = ["rt"]`), so a saturated fair band does not
  starve it; `issues/kernel/cpu-time-is-a-band-and-not-a-reservation.md`
  replaces that precedence with a reservation, which `watchdogd` then holds.
- **Nothing in the kernel is added for tests only** (owner ruling): a test
  harness uses this watchdog as it ships.
- On the T14 it is the PCH's TCO, which Linux's `iTCO_wdt` finds as "Intel PCH
  TCO device (Version=6, TCOBASE=0x0400)" and runs at a 30 s heartbeat. The
  T14 has no BMC or AMT (its i5-1135G7 has no vPro) and a battery no power
  switch cuts, so the TCO is its one reset that needs nothing of the kernel.
- A panic stops every feed, so an armed chipset ends the panic panel's hold
  within its bound, before `toyos_tco::PANIC_BOUND_MS` and whatever key is
  pressed (`kernel/src/drivers/panic_console/mod.rs:716-725`).

## Stages

1. **The measurement, on the owner's go-ahead.** A T14 boot arms the TCO and
   stops feeding it: `watchdog` with `tco-starve`, which `src/metal.rs`'
   `FLASHABLE` must first admit, and a job that holds the boot past the bound,
   since the shutdown disarms the timer (`kernel/src/syscall/machine.rs:88`).
   The machine resets within `toyos_tco::BOUND_MS` of its last feed with no
   hand on it, which is the exit of
   `issues/hardware/an-armed-tco-has-never-reset-the-t14.md`. If it does not,
   the track stops and goes back to the owner with that run's `loader.log`.

2. **`watchdogd` ships.** The shipped image arms the watchdog and starts
   `watchdogd`. The kernel feeds the timer from the scheduler pass until the
   claim is minted, and never after. A machine whose chipset `toyos_tco` has no
   row refuses the claim as absent, and the boot says it is unwatched.

   **Exit**: in QEMU, a `watchdogd` that stops feeding and a kernel wedged by
   `wedge-before-reset` each end in the chipset's reset; a fair-band load that
   resets a machine whose `watchdogd` lacks `rt` leaves one with `rt` fed; and
   the build refuses the class to any other row. On the T14, the wedged kernel
   ends in the same reset.
