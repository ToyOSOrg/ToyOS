---
status: open
kind: defect
opened: 2026-10-09
---

# A woken `klogd` can wait seconds on an idle CPU under HVF

A wake that makes `klogd` runnable on an AArch64 CPU under HVF can leave it
undispatched for seconds, until something else wakes that CPU.

**What it cost before the stop held the wire.** A boot asked to stop could
power off without `Shutting down.` on its console. The last word's drain
declined a held wire, and `serial::flush_final` spun on it and gave up.
Measured on 8-CPU `virt` guests under `Profile::Virt`, an Apple-silicon host
of 14 cores, QEMU 11.1.1:

- On the entropy stage's branch at `7522ec61e`, load 28 to 46:
  `virt_el1_smp`, `virt_mask_windows` and `virt_off_names_the_cpus_left_on`
  side by side with `virt_smp` and `virt_failed_ap_leaves_no_hole`, 131
  runs, five reds, each `STALLED: waiting for the boot's last word`; a
  sixth in a whole suite at load 41 to 47. Emulated at EL1 the same session,
  50 runs, no red. One red's PSCI trace held seven `CPU_OFF` and one
  `SYSTEM_OFF`, and its console stopped with `klogd` about 100 ms of guest
  clock behind the stop. The probe is `debug-shutdown-regs.patch` in
  https://github.com/ToyOSOrg/ToyOS/pull/802#issuecomment-6080912601.
- At `863d73ed3`, with a probe on the stop's two drains (on
  https://github.com/ToyOSOrg/ToyOS/pull/805), 2 reds in 21 runs of the same
  five, load 23 to 52. A red `virt_mask_windows`: at the decline the wire
  was free with one ticket outstanding (`ticket=197 now=196`), its turn
  posted to `klogd` and not taken, and the same 2.95 s later when
  `flush_final` gave up. A red `virt_el1_smp`: the wire free, `flush_final`
  spent 4.16 s.

**The dispatch delay itself.** `virt_reboot_wire_kept` on `Profile::Virt`
(two CPUs), at `863d73ed3` with the stop holding the wire, load 16: the
stop `post_wake`s `klogd` at guest 1.574 s, and `klogd`'s next loop top is at
6.579 s, on cpu1, where each of its tops that boot ran. The stop, on cpu0,
was parked the whole time. `klogd` ran 5 ms after the stop's deadline, when
a probe on that path kicked every CPU. That staging's wait timed out in 6 of
10 runs of that test.

**What it costs now.** The stop holds the console's wire from right after it
stops userland to the machine's end (`log::console::take_for_the_stop`), so
no record waits on `klogd` past it. A `klogd` not dispatched when the stop
asks costs the stop `log::console::LET_GO`, 5 s, and an alert, which reds
every test (`tests/common/serial.rs`, `NEVER_CLEAN`); where it never lets
go, the lines console holders had queued are not on the console, which the
alert says.

**Until it is fixed** these stay emulated (`tests/toyos.rs`):
`virt_mask_windows`, `virt_off_names_the_cpus_left_on`, `virt_reboot`,
`virt_reboot_wire_held` and `virt_reboot_wire_kept` at EL2, and
`virt_el1_smp` on `Profile::VirtTcg`.

**Not known.** Whether the wake reached the idle CPU, whether its sleep
handshake missed it, or whether HVF delivered the kick late: no register
capture of the idle CPU exists.

**Exit**: the cause named from a capture of the idle CPU, and fixed; then
`virt_el1_smp`, `virt_mask_windows`, `virt_off_names_the_cpus_left_on`,
`virt_reboot` and the two `virt_reboot_wire_` tests under `Profile::Virt` on
an Apple host, 300 side-by-side runs at load 30 or more with no red, the
harness's PSCI trace allowed under HVF; they then move to `Profile::Virt`,
`virt_reboot` keeping an EL2 row of its own for `SYSTEM_RESET` through the
SMC conduit, and `Profile::VirtTcg` is deleted.

Owner: the kernel's AArch64 bring-up, `issues/toyos-runs-on-arm64.md`, held
by the orchestrator's next kernel worker.
