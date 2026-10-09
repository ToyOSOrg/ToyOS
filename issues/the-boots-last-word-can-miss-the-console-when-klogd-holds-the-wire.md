---
status: open
kind: defect
opened: 2026-10-09
---

# The boot's last word can miss the console: the stop's drain declines while `klogd` holds the wire

A boot that was asked to stop can power off without `Shutting down.` on its
console. Seen on 8-CPU `virt` guests under HVF, six times, each a test red as
`STALLED: waiting for the boot's last word — it went quiet`: the guest was not
quiet but gone.

**What the code does, by reading.** `quiesce` (`kernel/src/syscall/machine.rs`)
logs the last word and calls `log::console::drain_inline`, which takes the
wire with `serial::try_wire` and returns at once where it is held: "whoever
holds the wire drains it too". The holder is `klogd`, on another CPU, part-way
through its backlog. Nothing waits for it. The caller goes on to the power-off
or the reset, which turns `klogd`'s CPU off before it reaches the record. The
stop's record and the census, logged above the last word, go the same way.

**What was measured.** The entropy stage's branch at `7522ec61e`, an
Apple-silicon host of 14 cores at 1-minute load 28 to 46, QEMU 11.1.1,
`tests/virtsmpcase` on eight CPUs under `Profile::Virt` (HVF), whose first job
`unmap_touch` has the kernel report eight faults, each at length:

- `virt_el1_smp`, `virt_mask_windows` and `virt_off_names_the_cpus_left_on`
  each wait for the last word. Side by side with `virt_smp` and
  `virt_failed_ap_leaves_no_hole`, 131 runs: five reds, two of `virt_el1_smp`,
  two of `virt_mask_windows`, one of `virt_off_names_the_cpus_left_on`. A
  sixth, `virt_el1_smp`, in the whole suite at load 41 to 47.
- The same five tests with `Profile::Virt` emulated at EL1 (`-cpu max`), the
  same session: 50 runs, no red. Five in 393 guests against none in 150 does
  not separate the two by itself (Fisher's exact test, p = 0.33): the place
  the reds stop in does.
- One red `virt_el1_smp`, captured: QEMU's `arm_psci_call` trace holds seven
  `CPU_OFF` and one `SYSTEM_OFF`, so the kernel powered the machine off. Its
  console's last line is the supervisor's `power: the machine stops, and
  logkeeper makes the log whole first (Shutdown)`, stamped 2.361, in among
  the kernel's fault-report records stamped 2.264: `klogd` was about 100 ms
  of guest clock behind when the stop began. No stop record, no census, no
  `Shutting down.`.
- The red `virt_off_names_the_cpus_left_on` said, after the wait began,
  `power: cpu6 is not off by PSCI's answer inside the budget; SYSTEM_OFF
  regardless` and the same of cpu7, which `arch::power::off` writes straight
  to the UART after `quiesce` has returned, and no `Shutting down.`.

The probe that captured it is a patch on the entropy stage's pull request.

**Not known.** Whether the wire's holder was `klogd`: no capture names it.
Whether an x86-64 guest with a 16550 loses its last word the same way.
Whether the two stalls of `virt_mask_windows` that
`issues/a-counters-read-under-host-load-can-go-silent-for-15-s.md` records
with no console, each `waiting for the boot's last word` under TCG, are this.
`virt_reboot` waits for `Rebooting.` through the same `quiesce` under HVF, on
a case that reports no fault before it, and has not been seen red.

**Until it is fixed** those three tests stay emulated (`tests/toyos.rs`), and
`Profile::VirtTcg` stays for `virt_el1_smp`.

**Exit**: the stop puts its last word on the wire before it takes a CPU down,
waiting for the wire's holder as `drain_for_the_stop` does, or the cause is
shown to be another from a capture. It is shown by a test that reds without
the fix on every boot, the wire held across the stop by an actuator, and by
the three tests under `Profile::Virt` on an Apple host: 300 side-by-side
runs at load 30 or more with no red. They then move to `Profile::Virt` and
`Profile::VirtTcg` is deleted.

Owner: the kernel's AArch64 bring-up, `issues/toyos-runs-on-arm64.md`, held
by the orchestrator's next kernel worker.
