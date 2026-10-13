---
status: open
kind: defect
opened: 2026-08-01
---

# The cpal ToyOS backend offers I16 alone

cpal's ToyOS backend (`toyos-0.18.0`) takes its rate and its release bound from
soundserver: `default_output_config` is the described device's,
`supported_output_configs` is mono and stereo over the client range soundserver
accepts, and `RELEASE_WITHIN` is derived from the fade, the ring and the device
period (#877). Its sample format is still I16 alone: a client asking for any
other format is refused.

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
