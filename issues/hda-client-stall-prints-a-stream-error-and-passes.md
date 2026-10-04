---
status: open
kind: tooling
opened: 2026-10-03
---

# `hda_client_stall` prints a stream error and passes

`tests/toyos-rust-tests/src/bin/hda_client_stall.rs` hands cpal an error
callback that prints `audio error: …` and does nothing else. The fork reports
two things through it: a stream whose signal pipe went away, and a dropped
stream soundd did not let go of within its fade and a ring of periods. Either
leaves the job exiting 0, and the `hda_client_stall` T14 row reads the job's
exit code. `tone.rs`'s `play_tone` asserts on the same callback.

The `testcases` readback of the T14 run of #638's head carries no
`audio error` line. Whether the T14 ever reports one under this job is not
measured.

## Exit condition

The job fails when its stream reports an error, and `hda_client_stall` passes
on a T14 run of that head.
