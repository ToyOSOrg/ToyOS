---
status: expected-red
kind: defect
opened: 2026-09-25
---

# A `quiesce_writers` writer's first write-and-fsync pass outlasts the job's 5 s spin-up

It asks for the reset only once each of its six writers has
finished one pass: a create, 64 KiB of writes and an fsync. If a writer is
still in its first pass after 5 s, the job prints `quiesce_writers: <n> of 6
writers reached their loop in 5s` and exits 1 without asking, so no stop
begins. Every sighting below then reads `QEMU never reported stopping: the
guest asked for a reboot and stayed up`; that misreport is
`issues/build/a-stopped-boot-whose-job-never-asked-waits-out-the-reset-budget-and-says-it-asked.md`.

`quiesce_dump_holds_the_stopped`, in the fast tier on the logd branch
(PR #492), dev host: red wide with `QEMU never reported stopping: the guest
asked for a reboot and stayed up`, then `ALONE ... GREEN`.

The boot's console shows the stick's transport breaking twice on `SCSI 0x2a`
(`no answer in the status phase in 2000 ms`, recovered each time), then
`quiesce_writers: 4 of 6 writers reached their loop in 5s` and
`test_rs_quiesce_writers exit=1`. The branch touches logd, netd and the
console, not the USB stack, quiesce or its config.

**Recurrence, PR #524's fast tier at `396f5b4d`, dev host.** Red wide after
325 s with the same `QEMU never reported stopping: the guest asked for a
reboot and stayed up`, then `ALONE ... GREEN` in 4 s. The capture has the same
`quiesce_writers: 4 of 6 writers reached their loop in 5s` and
`test_rs_quiesce_writers exit=1`, and this time no transport break on the
stick. While it ran, the host's 12 guest slots were full, shared with the
suites of five other worktrees (`toyos-lld`, `toyos-tcp`, `toyos-update`,
`toyos-logtrack`, `toyos-guiplat`).
The branch reorders xHCI ring writes (`dma_wmb`) but touches no quiesce code.
A same-session A/B then gave 5 of 5 green on each arm, main at `e48604c0` and
the branch: the red did not come back alone or beside the other arm, so it
is still not shown to be anything but load-bound.

**Recurrence, PR #542's nightly at 059c5de7 (run 36328646395, `guest (7)`),
KVM, one guest on the runner.** The same `QEMU never reported stopping: the
guest asked for a reboot and stayed up`, the same `quiesce_writers: 4 of 6
writers reached their loop in 5s` and `test_rs_quiesce_writers exit=1`, and no
transport break in the capture. A writer's first `quiesce-writer: <n> 1` line
is its first pass done: writer 1 at 0.681 s, 0 at 1.061 s, 3 at 1.844 s, 5 at
2.014 s, and writers 2 and 4 only at 6.610 s and 6.778 s, 5.8 s and 5.9 s
after their `<n> 0` lines. Writer 1 printed pass 33 at 6.905 s, its passes 93
to 306 ms apart. The branch changes no kernel or guest code. A red on a
one-guest lane is not the other suites' load.

**`quiesce_stops_the_machine`, PR #566's fast tier at `74f7d717`.** Writer 5
began its first pass at 2.170 s (`quiesce-writer: 5 0`) and ended it at
7.434 s (`5 1`), 5.264 s later. The other five had ended theirs by 3.738 s. At
6.874 s the job printed `quiesce_writers: 5 of 6 writers reached their loop in
5s`, and at 10.303 s the runner printed `===TEST_END test_rs_quiesce_writers
exit=1===`. The job never printed `6 writers are running; asking for the
reset`, and the capture has no `stop:` record. Red after 266 s.

The same test, with the same harness message and no `stop:` record:

- PR #524's fast tier at `235c5a5b`, load average 20 to 28: after 266 s, with
  `quiesce_writers: 3 of 6 writers reached their loop in 5s`.
- PR #555's nightly at `d2656765` (run 36351950439, `guest (3)`): after 47 s,
  with `4 of 6`.
- PR #511's merged head `a58abf50`: after 306 s, with a `usb-storage` transport
  break on `SCSI 0x2a` that recovered. Whether its job printed the give-up
  line was not recorded.

**Exit condition.** Re-enabled when a reproduction names what holds a writer's
first write-and-fsync pass for over 5 s while another writer passes in under a
third of a second, and the fix is shown against it. Owner: the `/log` write and
sync path `tests/toyos-rust-tests/src/bin/quiesce_writers.rs` drives; held by
the orchestrator.
