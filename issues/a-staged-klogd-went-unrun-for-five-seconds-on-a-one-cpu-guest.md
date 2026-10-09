---
status: open
kind: defect
opened: 2026-10-09
---

# A staged `klogd` went unrun for five seconds on a one-CPU guest

On one CPU, the stop parked and woke `klogd`, and `klogd` did not run for 5 s
of guest time. The stop's hand-off of the console's wire rests on the
opposite (`log::console::take_for_the_stop`): parked, the stop leaves its own
CPU to `klogd`.

**Measured once.** `machine_shutdown_wire_held` (q35, one CPU, x86-64 under
TCG on an Apple-silicon host of 14 cores), in the whole guest suite at
`c2a4542f0`, 1-minute load 21.53 when the suite began and 36.77 when it
ended. The staging's wait for `klogd` to take the wire, then bounded at 5 s
of guest time, panicked at guest 12.437 s, `the staged klogd never took the
wire`, 27 s of wall clock into a test that passes in about 7. Its console
was virtio-console's, which the harness keeps no copy of, so it does not
say whether `klogd` ran. Eighteen whole suites after
it, at load 18 to 67, did not repeat it.

**Who could run instead.** Only the stop was parked; `klogd` had been
woken, and userland was still live, since the staging runs before the stop
stops it. So userland holding the CPU is a cause as open as a lost wake or
a host that starved the guest.

**The capture.** The staging (`log::console::staged`) no longer has a bound
in guest time; `DEAF_CPU` into its wait it logs `klogd`'s scheduler state,
the stop's CPU, what that CPU runs and every task on its run queue, and goes
on waiting. Every staged test reds on that line
(`tests/common/power.rs`, `STAGED_KLOGD_UNRUN`), so a repeat is a red with
its capture, not a stall.

**Exit**: the cause named from that capture, and fixed; then
`machine_shutdown_wire_held` passes 200 side-by-side runs at 1-minute load
30 or more.

Owner: the orchestrator.
