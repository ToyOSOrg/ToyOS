---
status: open
kind: finding
opened: 2026-09-26
---

# `quiesce_wakes_on_the_last_park` lost its serial READY beside other guests

Fast tier at `98e803cb` (PR #510's branch; another worktree's DNS work was
building at the same time): `QEMU died before ===READY=== (status: exit 0)`.
The console carried the boot through `quiesce-last-park: quiesce-last is held
until the stop waits on it alone`, `stop: 4 of 7 userland thread(s) stopped
... in 2010 ms of a 2010 ms budget`, `usb-quiesce: disk 0 SYNCHRONIZE CACHE
ok` and `Rebooting.`, then `shutdown: /log did not answer in 2000ms`; the uart
captured `nothing at all`. The harness's re-run alone was green in 2 s.
`cargo run -- --known-red` answers NO.

The same shape as
`issues/kernel/quiesce-wakes-on-the-last-exit-lost-its-serial-ready-beside-other-guests.md`,
on the sibling arm.

**Exit**: a cause for the empty uart on a boot that rebooted as designed, or
the marker waited for where the boot's reboot cannot race it.
