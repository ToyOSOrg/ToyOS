---
status: assigned
kind: tooling
opened: 2026-09-28
---

# A stopped boot whose job never asked waits out the reset budget and says it asked

`stopped_boot` (`tests/common/power.rs`) serves the five quiesce tests that end
in a reset. It waits `qemu.budget(WAIT)` for QEMU's `SHUTDOWN` event and then
calls `returned_to_firmware` before it reads anything off the console. That
function turns every `None` into `QEMU never reported stopping: the guest asked
for a reboot and stayed up`. So a boot whose job exited without asking for the
reset waits out the whole budget and then reports that the guest asked.

In PR #566's fast tier at `74f7d717`, `quiesce_stops_the_machine`'s job exited 1
at 10.303 s without asking. The guest's scheduler then reported both CPUs idle
(`current=None`, `parked=2`) from 10.750 s through its last heartbeat at
253.244 s. The test went red after 266 s with that message. Every sighting in
`issues/kernel/a-quiesce-writers-first-pass-outlasts-the-jobs-five-second-spin-up.md`
carries the same message over a job that never asked.

**Exit**: `stopped_boot` stops waiting when the guest's job ends without
asking, and its red names that job's exit and last error line. A job that exits
before asking is then red in seconds, for its own reason. Owner:
`tests/common/power.rs`; held by the orchestrator.
