---
status: open
kind: tooling
opened: 2026-09-27
---

# Timing verdicts ruled off QEMU have no metal arm yet

A QEMU guest test asserts order, completion, content and counts, never how
long something took (owner ruling). The timing halves below were deleted from
the QEMU suite; each is a property of the T14, and no `METAL` row judges it
there yet. `latency_wake`'s p99, `tlb_shootdown_cost`'s tail,
`timer_calibration`'s ppm bound and `wake_storm_cost`'s linearity already have
one.

- **Audio deadlines.** No silence on the wire while a client streams — the
  capture's mid-tone gaps and soundd's `underruns` in `audio_tone`,
  `soundd_log_stall` and `hda_tone`, and the same under two CPU burners
  (`audio_tone_load`, deleted). The T14 carries no capture, so a metal arm reads
  soundd's own `underruns` and `late_wakes` off the stick's `/log`.
- **Every CPU reaches a scheduler pass each heartbeat period, and no window
  between two heartbeats is wide enough to hide a death** (`kernel_heartbeat`).
- **A fatal path halts the other CPUs before anything else**: none of their
  records stamped more than 100 ms past the fatal one
  (`panic_halts_the_others_first`, deleted).
- **A client out-writing a stalled peer costs netd no CPU**: under half a CPU
  busy over a 2 s window (`netd_stalled_peer`, deleted).
- **A wake is delivered rather than waited out**: 500 pipe round trips and a
  24-child, 24-thread exit storm inside 3 s (`blocking_read_stress`,
  `exit_wait_storm`), and every armed ring watcher woken inside 200 ms
  (`poll_wake_pipe`). These binaries ride the shared metal boot, so their
  bounds are gone there too.
- **With the acknowledgement delay disarmed, `munmap` returns under half the
  delay** (`tlb_shootdown_waits`' control on its own instrument).
- **A scheduler pass's cost distribution** (`sched_check_build`); no metal arm
  boots the check kernel.
- **A boot with no i8042 is no slower than one with it** (`i8042_absent`).
- **The USB connect settle ends on the device appearing and not at
  `EMPTY_BUS_NS`** (`xhci_slow_connect`); **a disk call ends inside
  `toyos_xhci::call::AFTER_BREAK`, a staged break skips its data-phase wait, and
  the offline ladder ends inside 2.75 s** (`tests/common/usb.rs`).
- **A null sink drains a client at the audio rate** (`metal_sim_null_audio`,
  which is `QemuOnly`).
- **init's stop waited for logd's flush answer**, read off the gap between init's
  stop line and the kernel's sync (`log_ring_keeps_the_owners_slots`); only
  init's own word is read now.

Not changed, and timing-dependent below the harness: `dump_nmi_probe`'s verdict
rests on the kernel's own 1 ms NMI answer budget and 250 ms kick budget.

**Exit**: each line above is judged by a `METAL` row, or ruled not owed.
Owner: the metal suite (`tests/toyos.rs`'s `METAL`); held by the orchestrator.
