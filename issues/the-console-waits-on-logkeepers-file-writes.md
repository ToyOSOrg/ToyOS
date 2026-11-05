---
status: open
kind: finding
opened: 2026-11-05
severity: [hang]
---

# The console waits on logkeeper's file writes

`logkeeper`'s loop is the only thing that reads the rings and grows the
replay the console's carriers and readers are fed from, and it writes and
syncs each round to `/log` itself (`Log::to_volume`, `Volume::write` and
`Volume::sync` in `system/logkeeper/src`), each a blocking call on the LOG
file server. `LOG_WRITE_BUDGET` judges a call only once it returns. So a LOG
server that takes a write and never answers, alive but wedged, leaves every
console channel silent for as long as it does, with no failure bound, and the
supervisor restarts only a server that ended.

Read from the code, not reproduced. The volume's open waited the same way
before it moved to a thread of its own, which is how the console stood silent
for the supervisor's 30 s declaration bound on `acpi_press_is_one_sci`'s boot.

**Exit**: no console channel waits on a `/log` call; a guest boot whose LOG
server stops answering mid-boot still carries its later lines. The shape: the
file is a consumer of the replay with its own offset, written on its own
thread as the readers and carriers are, and the stop asks it to reach the
replay's end and sync within a bound. #1190's per-file offset and bounded
stop catch-up (`Volume::through`, `catch_up`, `finish`) are its vehicle, so
whoever reconciles #1190 with this takes it up.

Owner: `system/logkeeper`; unheld.
