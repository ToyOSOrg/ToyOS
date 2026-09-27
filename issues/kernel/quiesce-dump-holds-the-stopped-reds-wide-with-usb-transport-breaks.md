---
status: expected-red
kind: defect
opened: 2026-09-25
---

# quiesce_dump_holds_the_stopped reds wide, with two USB transport breaks, and is green alone

Seen once, in the fast tier on the logd branch (PR #492), dev host: red wide
with `QEMU never reported stopping: the guest asked for a reboot and stayed
up`, then `ALONE ... GREEN`.

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

**Exit condition.** Re-enabled when a reproduction names what holds a writer's
first write-and-fsync pass for over 5 s while another writer passes in under a
third of a second, and the fix is shown against it. Owner: the `/log` write and
sync path `tests/toyos-rust-tests/src/bin/quiesce_writers.rs` drives; nobody is
holding it yet.
