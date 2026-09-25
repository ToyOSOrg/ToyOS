---
status: open
kind: finding
opened: 2026-09-25
---

# quiesce_dump_holds_the_stopped reds wide, with two USB transport breaks, and is green alone

Seen once, in the fast tier on the logd branch (PR #492), dev host: red wide
with `QEMU never reported stopping: the guest asked for a reboot and stayed
up`, then `ALONE ... GREEN`. `cargo run -- --known-red
quiesce_dump_holds_the_stopped` says it is not quarantined.

The boot's console shows the stick's transport breaking twice on `SCSI 0x2a`
(`no answer in the status phase in 2000 ms`, recovered each time), then
`quiesce_writers: 4 of 6 writers reached their loop in 5s` and
`test_rs_quiesce_writers exit=1`. The branch touches logd, netd and the
console, not the USB stack, quiesce or its config.

Exit: a cause, or a recurrence that shows it is not load-bound.
