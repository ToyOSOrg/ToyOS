---
status: open
kind: defect
opened: 2026-10-20
---

# cpal's close is bounded by one ring of device time, a verdict rather than a hang ceiling

cpal's ToyOS host (`src/host/toyos/mod.rs` in the cpal fork, branch
`toyos-0.18.0`, `Stream`'s `Drop`) waits for its stream thread to end, which
is the event soundserver's release of the closed stream causes, for at most
`release_within`: the stream's fade and one ring of periods at the device's
rate, about 29 ms at 44.1 kHz. A release that takes longer is reported through
the error callback as `soundserver did not let go of the closed stream within
…`, and `tests/guest/rust-tests/src/tone.rs` fails the job on it.

That bound is the device's playout time, not a hang ceiling. On QEMU 11.1.1's
`ich9-intel-hda` under TCG, periods complete in bursts of up to a ring paced by
the host (`max_batch=7`), and the bound failed `test_rs_audio_tone` and
`test_rs_audio_tone_resampled` at host load averages 22 and 101 with every
period completed and `underruns=0` (#950's evidence,
https://github.com/ToyOSOrg/evidence/tree/b0d5671935257fff8fff68c3b7d9d788ee718292/hda-claim/qemu). A loaded metal machine can be late the same
way. So the QEMU HDA row runs the master job only, and the tone jobs are
judged on the benches alone.

Owner: the cpal fork. Exit: the close waits for the stream thread's end with a
failure timeout sized as a hang ceiling, and a release that is merely slow is
not an error; the QEMU HDA row then runs `test_rs_audio_tone` too.
