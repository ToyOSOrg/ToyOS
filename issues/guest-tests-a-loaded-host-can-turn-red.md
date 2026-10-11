---
status: open
kind: tooling
opened: 2026-10-10
---

# Guest tests a loaded host can turn red

The owner's ruling: however loaded the host, no guest test goes red, and timing is valid only on
metal (`reviewer.md`, **Tests**). Each test below holds its verdict to a fixed wall-clock bound the
harness does not scale, inside a guest whose vCPUs a loaded host deschedules. Found by reading
each one's clock; none of these bounds has been seen red under load, so each is a mechanism and a
measurement owed. The one observed red of this shape, `virtio_sound_counts`, is already deleted.

- `usbd_drives_the_spare` (`usbd_spare.rs`, `CLAIM_RETURN` 2 s and the 2 s halt-or-reset assert):
  a killed usbd's claim back, and the controller halted or reset, each within 2 s.
- `kill_ends_every_wait` (`kill_ends_every_wait.rs`, `HELD` 1 s): a kill that takes more than 1 s
  of starved wall time to land reads as "the child outlived" it. A timing claim.
- `sched_stress` (`sched_stress.rs`): the post-wake vruntime lag held to ±50 ms after a 10 ms
  sleep, where vCPU time the host takes is charged as the thread's runtime. Its connect-storm pace
  is `issues/timing-verdicts-ruled-off-qemu-have-no-metal-arm.md`'s.
- `abuse_thread_table` (`abuse_thread_table.rs`, `RUN_BOUND` 10 s): each thread runs within 10 s.
- `acpi_mediated_access` (`acpi_mediated.rs`, `RELEASED` 5 s): a dead holder's claim back within
  5 s.
- `netstack_socket_churn` (`netstack_socket_churn.rs`, `LET_GO` 20 s), `netstack_streams` and
  `netstack_streams_e1000e` (`netstack_streams.rs`, `WOKEN` 60 s, and the harness's 30 s
  `ANSWERED` in `tests/toyos.rs`).

The harness's liveness ceiling (`tests/common/qemu.rs`, `host_scale`, `budget_smp`) is scaled by
the fastest boot the run has seen, so load that arrives after it is not in it.
`issues/a-netcase-boot-under-host-load-said-nothing-past-the-firmwares-screen-clears.md` records
`netstack_socket_churn` and `netstack_streams` each red at boot, before any bound above ran, at
one-minute load averages of 82.78 and 69.67 on 14 cores as their suites began; the first had its
ceilings priced at 1.00x. The stop's wall-clock budget is owned by
`issues/timing-verdicts-ruled-off-qemu-have-no-metal-arm.md` and
`issues/a-woken-klogd-can-wait-seconds-on-an-idle-cpu-under-hvf.md`.

Owner: the orchestrator, which dispatches the reshape.

**Exit condition**: each test named above waits on its event under the harness's scaled hang
ceiling, and a timing claim among them (`kill_ends_every_wait`'s `HELD`) is judged by a `METAL`
row; and the liveness ceiling is priced on the host's speed while each test runs, not on the
fastest boot the run has seen.
