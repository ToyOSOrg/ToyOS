---
status: open
kind: defect
opened: 2026-08-01
---

# The cpal ToyOS backend hardcodes 44100/2ch/i16 and rejects everything else

soundd's resampler and channel-conversion paths are unreachable from any real
client and therefore effectively untested. The backend also `assert_eq!`s the
device rate against a compile-time constant, so changing the driver's rate
aborts every cpal app.

**`Drop`'s bound on soundd is built from copies the same way.** A dropped
`Stream` waits for soundd to close its signal pipe for at most soundd's 5 ms
fade, in whole periods, plus the 8 periods of the stream's ring: 1280 frames at
44100 Hz (`RELEASE_WITHIN`). Past it, `Drop` refuses by name and returns. The
fade (`toyos_mixer::ramp_frames`) and the ring depth (soundd's `slot_count`, see
`issues/client-ring-depth-is-the-devices-pipeline-depth.md`) are copies:
`StreamOpenResponse.slot_count` reaches `AudioStream::open` and stays private,
and no message carries the fade. If soundd's fade grows or a device's pipeline
deepens, the bound shrinks under what soundd needs, and a healthy soundd is
refused. That part goes when `AudioStream` says the ring depth and the fade and
the host derives `RELEASE_WITHIN` from them, which changes `toyos/src` and the
audio protocol.

Deferred to the quiet-tree window, not neglected: editing that fork needs
`.cargo/config.toml` path overrides, which redirect cpal for **every** agent in
the tree. Same scheduling constraint as the fork lint audit.

**Client liveness is blocked on this, not on soundd.** The ambiguity between a
paused and a wedged client is designed in: pausing is the client's stream
thread simply not reading its signal pipe, with no coordination of any kind,
and the cpal backend's `pause()` is a purely local futex store soundd is never
told about. No change confined to soundd can separate the two, and landing the
soundd and SDK halves alone would kill every paused cpal client. The
**protocol**, not the implementation, is what needs to change;
`issues/a-client-cannot-tell-soundserver-it-paused.md` is that change.

The assignment was reclaimed 2026-08-23: nobody holds it, and the block above is
what it is waiting on.
