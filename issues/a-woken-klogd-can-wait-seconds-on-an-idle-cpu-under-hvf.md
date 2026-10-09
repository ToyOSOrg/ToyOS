---
status: open
kind: defect
opened: 2026-10-09
---

# A woken `klogd` can wait five seconds on an idle CPU under HVF

A wake that makes `klogd` runnable on an idle AArch64 CPU under HVF can leave
it undispatched until something else wakes that CPU.

**Measured.** `virt_reboot_wire_kept` on `Profile::Virt` (HVF, two CPUs),
the entropy stage's branch at `863d73ed3` with this fix applied, an
Apple-silicon host of 14 cores, 1-minute load 16 at the one reading taken:
the stop sets the
staging's flag and `post_wake`s `klogd` at guest 1.574 s, and `klogd`'s next
loop top is at 6.579 s, on cpu1, as each of its six tops that boot was. The
stop, on cpu0, was parked on the staging's watch the whole time. `klogd` ran
5 ms after the stop's deadline, when a probe on that path had kicked every
CPU. The staging's wait timed out in 6 of 10 runs of that test, and in none
of 4 of `virt_reboot_wire_held`, whose staging is the same code to that
point. The probe is a patch on the pull request that deleted
`serial::flush_final`.

The same shape, read off the probe on that branch's own kernel: a stop that
let the console wire go to a queued `klogd` found its turn posted and still
not taken 2.95 s later, on an 8-CPU guest
(`issues/a-counters-read-under-host-load-can-go-silent-for-15-s.md`).

One occurrence that may be it, on x86-64: `machine_shutdown_wire_held` (q35,
one CPU, TCG) in the whole suite at `c2a4542f0`, 1-minute load 21.53 when the
suite began and 36.77 when it ended, on the same host: the staging's wait,
then bounded at 5 s of guest time, panicked, `the staged klogd never took the
wire`, 27 s of wall clock into a test that passes in about 7. Its console was
virtio-console's, which the harness keeps no copy of, so it does not say
whether `klogd` ran or the host starved the guest. Eighteen whole suites after
it with a probe on that wait, at load 18 to 67, did not repeat it. The
staging's wait now has no bound in guest time, so a repeat is a stall the
harness's ceiling names.

**What it costs now.** The stop waits `log::console::LET_GO` for `klogd`,
then alerts and writes over it; nothing is lost. A program woken the same
way waits the same.

**Not known.** Whether the wake reached cpu1 at all, whether cpu1's sleep
handshake missed it, or whether HVF delivered the kick late; no register
capture of cpu1 exists. Whether TCG or x86-64 can do the same: the staged
tests run under TCG (`Profile::VirtEl2`) and on q35, and have not.

**Exit**: the cause named from a capture of the idle CPU, and fixed;
`virt_reboot_wire_kept` and `virt_reboot_wire_held` move to `Profile::Virt`
and pass 100 runs each under HVF with no staging timeout.

Owner: the orchestrator.
