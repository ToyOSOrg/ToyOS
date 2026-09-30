---
status: open
kind: defect
opened: 2026-09-30
---

# cpal's ToyOS `Drop` bounds its wait on soundd with two numbers soundd never sent

A dropped cpal `Stream` waits for soundd to close its signal pipe for at most
soundd's 5 ms fade, in whole periods, plus the 8 periods of the stream's ring:
1280 frames at 44100 Hz (ToyOSOrg/cpal `toyos-0.18.0`, `src/host/toyos/mod.rs`,
`RELEASE_WITHIN`). Past it, `Drop` refuses by name and returns.

The period and the rate are soundd's answer to the open, and the host asserts
them. The fade (`toyos_mixer::ramp_frames`) and the ring depth (soundd's
`slot_count`, see `client-ring-depth-is-the-devices-pipeline-depth.md`) are
copies: `StreamOpenResponse` carries `slot_count` and `AudioStream` keeps it to
itself, and nothing carries the fade. If soundd's fade grows or a device's
pipeline deepens, the bound shrinks under what soundd needs, and a healthy
soundd is refused.

## Exit condition

`AudioStream` says the stream's ring depth and soundd's fade, the cpal host
derives `RELEASE_WITHIN` from them, and this file is deleted.
