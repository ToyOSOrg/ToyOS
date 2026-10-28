---
status: open
kind: defect
opened: 2026-08-01
---

# The cpal ToyOS backend offers I16 alone

cpal's ToyOS backend (`toyos-0.18.0`) takes its rate and its release bound from
soundserver: `default_output_config` is the described device's,
`supported_output_configs` is mono and stereo over the client range soundserver
accepts, and each stream's `release_within` is derived from the fade, the ring
and the device period (#877). Its sample format is still I16 alone: a client asking for any
other format is refused.

**Client liveness waits on the same fork, not on soundserver alone.** The
ambiguity between a paused and a wedged client is designed in: pausing is the
client's stream thread simply not taking its signal event, with no
coordination of any kind, and the cpal backend's `pause()` is a purely local
store soundserver is never told about. No change confined to soundserver can
separate the two, and landing the soundserver and SDK halves alone would kill
every paused cpal client. The **protocol**, not the implementation, is what
needs to change; `issues/a-client-cannot-tell-soundserver-it-paused.md` is
that change.

Nobody holds it.
