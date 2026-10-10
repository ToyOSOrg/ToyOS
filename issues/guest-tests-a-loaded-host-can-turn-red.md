---
status: open
kind: defect
opened: 2026-10-10
---

# Guest tests a loaded host can turn red

The owner's ruling: however loaded the host, no guest test goes red, and timing is valid only on
metal (`reviewer.md`, **Tests**). Each test below holds its verdict to a fixed wall-clock bound the
harness does not scale, inside a guest whose vCPUs a loaded host deschedules. Found by reading
each one's clock and sleeps; **none was run under load**, so each is a mechanism, not an observed
red. The one observed red of this shape, `virtio_sound_counts`, is already deleted.

- `usbd_drives_the_spare` (`usbd_spare.rs`, `CLAIM_RETURN` 2 s and the 2 s halt-or-reset assert):
  a killed usbd's claim back, and the controller halted or reset, each within 2 s.
- `kill_ends_every_wait` (`kill_ends_every_wait.rs`, `HELD` 1 s): a kill that takes more than 1 s
  of starved wall time to land reads as "the child outlived" it. A timing claim.
- `sched_stress` (`sched_stress.rs`): the post-wake vruntime lag held to ±50 ms after a 10 ms
  sleep, where vCPU time the host takes is charged as the thread's runtime; its connect storm
  rests on a flat 50 ms sleep for the acceptor to be serving.
- `abuse_thread_table` (`abuse_thread_table.rs`, `RUN_BOUND` 10 s): each thread runs within 10 s.
- `acpi_mediated_access` (`acpi_mediated.rs`, `RELEASED` 5 s): a dead holder's claim back within
  5 s.
- `netstack_socket_churn` (`netstack_socket_churn.rs`, `LET_GO` 20 s), `netstack_streams` and
  `netstack_streams_e1000e` (`netstack_streams.rs`, `WOKEN` 60 s, and the harness's 30 s
  `ANSWERED` in `tests/toyos.rs`).

Two harness-wide mechanisms of the same shape, also unmeasured: the guest kernel's stop waits a
wall-clock budget for `klogd` and every thread to park, which host scheduling already decided
under HVF (`issues/a-woken-klogd-can-wait-seconds-on-an-idle-cpu-under-hvf.md`); and the liveness
ceiling (`tests/common/qemu.rs`, `host_speed`, `budget_smp`) is scaled by the host speed measured
at boot, so load that arrives mid-run is not in it.

Owner: the orchestrator, which dispatches the reshape.

**Exit condition**: each test named above waits on its event under the harness's scaled hang
ceiling, or is deleted where its claim is a timing claim (metal's), and the flat 50 ms sleep is a
wait on the acceptor.
