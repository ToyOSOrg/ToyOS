---
status: open
kind: tooling
opened: 2026-09-27
---

# Timing properties QEMU no longer judges, and the clocks it still waits on

A QEMU test measures no time; the harness's hang ceiling is its only clock
(owner ruling). The properties below were deleted from the QEMU suite with
their clocks, and no `METAL` row judges them.

**Owed a metal row:**

- **Audio.** soundd's `underruns` stay 0 under two CPU burners
  (`audio_tone_load`). `desktop_audio_client`'s three verdicts: a desktop client
  through the null sink exits, a second is taken up while the first streams,
  and the desktop still answers afterwards. A null sink drains a client at the
  audio rate (`metal_sim_null_audio`). soundd submits at least a period for
  every period's worth of frames a client handed it
  (`inspect_reads_its_owners`: `sound.periods.submitted × sound.period_frames`
  against what `inspect_plays` proved taken).
- **Boot.** The loader's and the kernel's TSC account of a boot agrees with the
  host's clock over the same boot (`boot_from_power_on`).
- **Scheduling.** Every CPU reaches a scheduler pass each heartbeat period, and
  no window between two heartbeats hides a death (`kernel_heartbeat`). A
  scheduler pass's cost distribution (`sched_check_build`; no metal arm boots
  the check kernel). A stop is woken by its last park before its budget,
  rather than giving up on it (`quiesce_wakes_on_the_last_park`).
- **Wakes delivered rather than waited out**: 500 pipe round trips and the
  24-child, 24-thread exit storm inside 3 s, every armed ring watcher woken
  inside 200 ms (`blocking_read_stress`, `exit_wait_storm`, `poll_wake_pipe`).
  These ride the shared metal boot without their bounds. A kept, closed window
  takes no wakes while its application idles (`toolkit_winit_loop`'s stage 6).
- **Accounting.** A refused syscall is charged to its caller's CPU time
  (`process_stats`). A client out-writing a stalled peer costs netd under half
  a CPU (`netd_stalled_peer`). blockd's bench throughput (`blockd_io`).
- **Input.** A boot with no i8042 is no slower than one with it
  (`i8042_absent`). The i8042 counters repeat at most once per 10 s and only
  when the pin asserted (`i8042_health_cadence`), and the idle loop does not
  spin on the controller (`i8042_health`'s idle trips). The fatal path's panel
  holds while a key is held (`panic_key_holds`).
- **USB.** The connect settle ends on the device appearing and not at
  `EMPTY_BUS_NS` (`xhci_slow_connect`). A disk call ends inside
  `toyos_xhci::call::AFTER_BREAK`, a staged break skips its data-phase wait, the
  offline ladder ends inside 2.75 s, and a port reset that never verified spent
  its rung (`tests/common/usb.rs`). A controller that will not halt and a port
  that will not reset are refused only after the 2 s budget was waited out
  (`xhci_deaf_registers`).
- **Network.** netd's connection cap counts connects still pending alongside
  established ones (`netd_connection_caps`, which now fills the cap with
  established connections only). A burst of silent connections is bounded:
  netd keeps some and not all (`netd_hostile_peer`).
- **Logging.** init's stop waited for logd's flush answer
  (`log_ring_keeps_the_owners_slots`; only init's own word is read now). logd
  lets a network reader that stopped reading go within its `STALLED`
  (`log_stream_stalled_reader`). logd writes `/log` promptly while the machine
  runs (`kernel_log_file`, and `tests/common/usb.rs`'s stick with no write
  cache).
- **Panel and storage.** A dump survives the compositor's next repaint
  (`screen_blocked_dump`). A same-length overwrite of a pinned `/home` file
  reads back whole through its displaced file's teardown: one read is taken
  now, and it may come before the teardown (`home_overwrite_reads_back`).

**Premises: a QEMU test still waits on a clock that decides no verdict**, where
the event it stands for has no word a test can read:

- Paces that give a stimulus time to land: the input pacing of
  `metal_sim_window_drag` and of the i8042 keyboard arms,
  `metal_sim_pointer_churn`'s `SETTLE`, the xHCI hotplug and flap sleeps in
  `tests/common/usb.rs`, `sched_stress`'s connect-storm pace, `locale_gate`'s
  poll, the compositor and IPC probes' paces, `ftruncate_flush_race`'s
  `INTO_WINDOW`, and `redirty_mid_flush`'s swept delay.
- Drains of work handed to another thread: the 200 ms `iod` drains in
  `writeback_durability` and `home_backing_revoked`, `fat_backing_revoked`'s
  drain, and `log_gate`'s `STORM_SETTLE` and `QUIET_READS`.
- Product clocks a QEMU boot races: `boot_deadline_ends_a_wedge`'s 15 s
  deadline against the job list reaching its shutdown,
  `hard_lockup_ends_a_deaf_cpu`'s 30 s, `loader_watchdog_arms` reading
  `timeout=0` before the TCO expires, and the stop's 2010 ms budget (a stop
  that gives up is not judged in QEMU).

**A clock that decides a verdict**: `census_wait::settled` takes the census
once two readings 10 ms apart agree. On a starved host the first can be read
mid-release, so a leak check anchored on it (`handle_kill_policy`,
`handle_lifetime`, `shm_release_reclaims`) can pass wrongly.

**Exit**: each owed line is judged by a `METAL` row or ruled not owed by the
owner, each premise waits on the event it stands for or is ruled acceptable,
and the census is read on the release it stands for. Owner: the metal suite
(`tests/toyos.rs`'s `METAL`); held by the orchestrator.
