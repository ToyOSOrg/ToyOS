---
status: expected-red
kind: tooling
opened: 2026-10-01
---

# A harness wait reads a starved guest as a stopped one

Every wait the harness puts on a guest is measured on the host's wall clock.
On a loaded host a TCG guest's QEMU sits runnable and unserved, so a guest that
is slow reads as one that stopped, and the host's load decides the verdict:

- `update_boots_the_new_kernel`, `update_falls_back_from_a_dying_kernel`,
  `update_floor_is_the_images_own` and `update_refusals_boot_the_other_slot`
  STALLED in the same run (`648-648-whole.log`, `wt/toyos-proclife1`
  `60ec86df3`, load average 84 on 14 cores): "the console and the 16550 both
  went quiet for 15 s", each just after the loader's "this pass resets the
  machine". `await_machine` (`tests/common/update.rs`) ends on 15 s of silence
  alone, and the firmware between that reset and the loader's next line is
  silent. The same run's `boot_from_power_on` read "power-on to loader 17387
  ms", against 5021 ms on a quiet host. `update_boots_the_new_kernel` STALLED
  the same way on `wt/toyos-proclife` `6e9d6a4df` (`642r2-642-whole.log`).
- `guest_dies_with_its_harness` (same 648 run, 21 s): "Boot timed out waiting
  for ===READY===; the console carried: nothing at all". The owner process has
  measured no boot, so its `wait_for_ready` ceiling is the unscaled 20 s, and
  its 16550 had reached "ROOT: read into memory" — a guest still working.
- `650-libcllvm-whole.log` (`wt/toyos-libcllvm`, libc only, load average
  66–74, image builds in the same run): `update_refusals_boot_the_other_slot`
  and `update_boots_the_new_kernel` STALLED as above, and `loader_watchdog_arms`
  timed out its boot at 132 s of wall clock — 10 s × 12 wide × `host_scale` —
  with the console at "BdsDxe: starting Boot0001". Its 50-odd other runs in
  the kept logs passed in 6–34 s. Whether that guest was starved or the loader
  stalled before its first line is what a wait on the guest's own time
  separates. In the serial tail of the same run, `metal_sim_window_drag` and
  `usb_boot_stick_pulled` timed out their boots at 21 and 22 s — the 20 s a
  phase of one guest gets — with the loader still printing its segments. The
  run reported "fastest boot 1331 ms against the reference 1320 ms — liveness
  ceilings paid at 1.01x" at a load average of 66–87: the fastest boot is
  taken at the run's quietest moment, so `host_scale` cannot see the load.

The opposite error has the same cause. `budget` multiplies a test's ceiling by
the phase width and by `host_scale`, two stand-ins for the host's load, so a
guest that stops at once is waited on for 37 times its budget:
`redirty_mid_flush` (`642r2-642-whole.log`) said nothing after its first second
and was called stalled at "4474s of guard expired": its 120 s × 12 wide × a
`host_scale` of 3.1 (4474 / 1440), the fastest boot the run had seen when the
test began.

**Exit**: every wait on a guest reads time the guest had, with the host's
steal taken out, and no ceiling scales by the host's load; then these rows go.
