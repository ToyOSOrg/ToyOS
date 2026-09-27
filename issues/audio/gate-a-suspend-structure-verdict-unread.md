---
status: open
kind: defect
opened: 2026-08-21
---

# Gate A refused its own instrument on 2026-08-17 and nobody read the verdict

`gate-a.yml`'s 2026-08-17 nightly (run `31992902784`) stopped shard 1 at
iteration 25 of 30 with the thorough tier's hardest verdict — not a statistic,
the instrument declining to measure:

```
[gate A] FAILED on iteration 25: audio_tone.smp8 instrument broken: suspend
structure: no `soundd: suspended` after the last client removal; suspend
structure: no `virtio-sound: stream 0 stopped` after the last client removal —
the device is still running with no clients
```

`soundd: suspended` and
`virtio-sound: stream 0 stopped` are the two lines that say the idle path
released the device; their absence says a boot left the device running with no
clients, which is the subject of `stop-the-device-voice-keep-the-wake` and
`idle-suspend-reds-on-a-loaded-host-and-on-main` from the other side. One
occurrence in 25 iterations is a rate nobody has.

**The evidence expires.** `/tmp/gate-a.log` is uploaded per shard with
`retention-days: 30`, so run `31992902784`'s artifacts go on 2026-09-16; the job
logs outlive them. Everything quoted above is already here for that reason.
