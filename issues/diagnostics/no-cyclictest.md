---
status: open
kind: track
opened: 2026-08-08
---

# There is no cyclictest, so nobody can ask this machine what its wake latency is

`git grep -in cyclictest` over the tree finds it only in this file. Until a
cyclictest-equivalent exists, **no honest claim can be made about this machine's
wake latency in either direction** — which makes it the instrument that turns
the first metal boot into a measurement rather than an impression.

**What to build.** A userland program that enters the RT band, arms an absolute
timer, sleeps, and histograms `actual − programmed` at 1 µs resolution over
enough samples to have percentiles rather than a maximum.

**What it needs, and it is not blocked on any of it.** `SYS_RT_ENTER` (112) is
the privilege: a `SysCap` carrying `Rights::RT`, granted by a `system.toml` row
(`[programs.soundd] syscap = ["rt"]` is the shape), gated at the dispatch site
and not in the scheduler. The gate this file was opened against is gone —
`SYS_SET_RT_PRIORITY` (96) demanded a `VirtioSound` or `HdaAudio` device claim,
so a latency tool could only reach the band by taking the sound card away from
soundd and measuring a different machine. Number 96 is retired, and
`toyos-abi/src/syscall.rs` says why: "a claim is not a privilege".

**What exists is not a substitute, and each instrument fails differently.**
soundd's `max_wake_lat_ns` (`toyos-mixer/src/stats.rs`, printed by
`userland/soundd/src/mix.rs`, read by gate A in `tests/common/audio.rs` and
baselined in `tests/audio-baseline.toml`; the thorough tier runs Mann-Whitney on
`max_wake_lat_us`) is a **max over a ~2 s window, not a distribution** — no
percentiles, no sample count; it measures against a DLL's *prediction of a DMA
completion* rather than against a programmed timer, so it folds in the device
model; and it needs soundd plus a sound card to exist at all, which is exactly
what the T14 has not got. `toyos-sched`'s invariant I4
(`toyos-sched/sim/src/invariants.rs`) bounds the same quantity but runs in the
simulator, so it can never see TCG distortion, real IPI delivery, or metal.
