---
status: open
kind: tooling
opened: 2026-10-03
---

# The tone client waits a flat 200 ms for its tail, and for its end with no bound

`tests/toyos-rust-tests/src/tone.rs`'s `play_tone` is what `test_rs_audio_tone`
and `test_rs_soundd_log_stall` play, so the `hda_tone` and `soundd_log_stall`
T14 rows ride it. It sleeps 50 ms at a time until its callback has written the
tone's last sample, with no bound of its own, and then sleeps a flat 200 ms for
the tail to drain through soundd and the device before it drops the stream.

Root `CLAUDE.md` allows neither: a wait is on the event, bounded by a timeout
that fails loudly.

## Exit condition

`play_tone` holds no sleep: what it waits on is an event, bounded by a timeout
that panics by name, and `hda_tone` and `soundd_log_stall` pass on a T14 run
of that head.
