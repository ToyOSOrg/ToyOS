---
status: open
kind: defect
opened: 2026-09-01
---

# `ftruncate_flush_race` reports a serialisation failure when its stimulus missed the window

`tests/toyos-rust-tests/src/bin/ftruncate_flush_race.rs` races a truncate
against a flush the `ftruncate-flush-stall` actuator holds open for 400 ms, and
concludes "the resize does not serialise with the metadata window" when no
attempt waited 150 ms. Its own docstring says the loop is one-sided because a
sleep can overshoot — but nothing checks which of the two happened, and the
kernel already prints the answer.

Every failing attempt carries

    [kernel 0.800 cpu1 tid=1] vfs: STALLED WINDOW HELD — /log/truncate-race.bin still 12288 bytes after 400ms

`HELD` means the size did not change inside the window, so the truncate was not
there to be serialised against: the guest's `thread::sleep(50ms)` expired after
the 400 ms stall had already ended, on a guest whose other vCPU was spinning
preemption-off for that whole stall. A resize that really did not serialise
would show `BROKEN` instead. The harness holds this log
(`tests/common/volumes.rs::ftruncate_flush_race`) and does not read it, which is
`tests/CLAUDE.md`'s "a stimulus sent through a channel that can silently lose it
is verified before its effect is asserted".

Measured on the dev host (14 cores), same session, against the branch that
became this entry and against the same tree with its kernel change reverted:

| arm | condition | runs | result |
|---|---|---|---|
| branch | alone | 6 | 6 pass, waits 355.1 / 356.2 / 357.5 / 358.2 / 358.6 / 369.4 ms |
| kernel change reverted | alone | 3 | 1 fail, then 2 pass (attempt 0, 358.0 and 356.3 ms) |
| branch | full nightly tier | 3 | 2 fail, 1 pass |
| kernel change reverted | full nightly tier | 1 | pass |

It fails on both arms and it passes on both arms, so the A/B is not a verdict
about a diff; it is a rate. `cargo run -- --known-red ftruncate_flush_race` says
`NOT ON THE LIST`.

The fix is not a wider `CONTENDED` and not more attempts. The test has to know
whether its stimulus landed: the harness reads the actuator's own `HELD` /
`BROKEN` line and reports "the truncate never reached the window" separately
from "the truncate reached it and did not wait". Only the second is the defect
this gate exists for; the first is a retry.
