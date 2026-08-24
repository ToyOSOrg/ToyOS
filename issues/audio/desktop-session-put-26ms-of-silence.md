---
status: open
kind: finding
opened: 2026-08-07
---

# A desktop session put 26 ms of silence on the wire, and gate A has never measured this workload

The owner ran `cargo run` on 2026-08-07 (guest RTC 13:14:51 UTC, tree at or
after `43ce73e`), started doom from the terminal, let its demo loop run to
t=69 s, then ran `tone` 44 times. 391 s of serial, 119 soundd stats windows.
Every number below is from that capture.

**Harm, by the fast tier's own definition.** Two windows during doom:

```
soundd: wakes=537 completions=675 submitted=675 underruns=8 drains=1 max_wake_lat_us=86779 max_batch=8 clients=1 deferred=33
soundd: wakes=389 completions=690 submitted=690 underruns=1 drains=2 max_wake_lat_us=92175 max_batch=8 clients=1 deferred=7
```

Nine periods — 26 ms — submitted with a client streaming and no client audio
behind them (`MixStats::period` in `toyos-mixer/src/stats.rs`, which is where
that counter moved when the mixer's decisions became a pure crate).
`tests/audio-baseline.toml` records `underruns` 0
on all 120 runs of its sample, and the fast tier's verdict is exactly this
counter. There is no capture to corroborate it: `--dump-audio` was not on.

**And it is not confined to those two windows.** Across the whole run, 15 of
119 windows report `drains` (22 events; recorded sample 0/120), and the
`max_wake_lat_us` distribution never once enters the recorded range:

```
                     n     min     p50     p90     max
doom phase          31   21167   30367       —  106654   (4.59 pipeline depths)
tone phase          88   18116   21896   24079   63664
audio_tone sample   30    5666       —       —   10090   (baseline file)
```

The tone phase is 88 windows, none below 18116 us, against a recorded sample
whose *worst of 30* is 10090. The two distributions are disjoint. 106654 us is
past the `audio_tone_load.smp8` ceiling of 80000 (this guest is `--smp 8`).

**Whose lateness it is, is not the same question in the two phases, and the
`deferred` column separates them.** `deferred` counts a mix cycle declining to
submit because a streaming client's ring was empty and there was still playout
margin (the `mid_refill` arm of `mix_thread`, `userland/soundd/src/mix.rs`) —
soundd's restraint, waiting for a producer:

```
doom phase   173 deferrals across 14 of 31 windows
tone phase     1 deferral  across  1 of 88 windows
```

Same soundd, same device, same kernel. So the doom-phase underruns are **doom
failing to fill its ring**, held off by soundd until the floor and then paid in
silence — not the audio path being late. The 92175 us window sits beside a
compositor window of `frames=23` where the steady state is 65-70, so doom
stopped presenting at the same moment it stopped producing; both recovered
within one window. `tone` is a trivial producer and never does this.

That leaves the tone phase as the clean signal, and it has `deferred` 1,
`drains` 3 and `underruns` 0 across 88 windows — nothing wrong with the audio
at all, and a wake-lateness figure 2-4x the recorded sample anyway. Which is
why the measurement itself is the first thing to rule out.

**Two things make this different from the gate A entries beside it**, which are all
gate A reddening on its own configuration:

- The client is doom — a real producer with a SoundFont synthesizer thread —
  and there is a compositor blitting 200-450 MB/s to the scanout beside it.
  Nothing in gate A resembles that.
- The 44 `tone` runs exercise **suspend → resume**, 44 times. Gate A's single
  client never leaves and comes back, so the resume path has no coverage at
  all. Every resume here costs a ~22 ms lateness sample, and 22 ms is 0.94 of
  one pipeline depth (23219 us), which is what a wake measured against a
  prediction one whole pipeline stale would look like.

**Read the tone-phase cluster carefully before treating it as load.** It is far
too tight to be scheduling noise (min 18116, p50 21896, p90 24079 over 88
windows, one per stream start). One candidate mechanism, offered as a
hypothesis and not a measurement: `signal_clients`' caller arms its wait on
`target` — the *next future* grid point when the DLL estimate is past due — but
records `armed_on = Some(t_est)`, the stale estimate, so the sample is taken
from an instant soundd deliberately did not ask to be woken at and includes
every whole period it skipped.

**Where that code is now, and what has moved under it.** The mix loop left
`userland/soundd/src/main.rs` for `mix_thread` in `userland/soundd/src/mix.rs`;
the arming block and `armed_on = Some(t_est)` survive there unchanged, and so
does the sample taken against `armed_on`. Two things have changed around it and
neither settles the question:

- The site now argues the choice rather than only asserting it: reading the DLL
  estimate at wake would score a window's first wake — armed while soundd was
  idle and asking for no wake time at all — as a missed deadline, so the sample
  is deliberately the distance from the prediction, "however large". That is an
  answer about `t_est` versus the DLL's later estimate. It is not an answer
  about `t_est` versus `target`, which is the instant the timer was actually
  armed for, and which is what this entry asks about.
- Since 2026-08-21 the statistic is recorded in two halves, split at the oldest
  record's ISR timestamp: `worst_irq_late_us` is the device failing to complete
  when it was due and `worst_pickup_us` is soundd failing to run once it had.
  Both are clamped to the same `t_est`, so the split re-attributes the number
  above without changing what it is measured from — and the `max_wake_lat_us`
  distribution quoted in this entry is a pre-split statistic.

Whoever takes this should settle `t_est` against `target` before reading any of
the numbers above as a property of the scheduler.

**Reproduction.** `cargo run`, `doom` in the terminal, wait for the demo loop,
quit, then `tone` repeatedly. Add `--dump-audio` so the wav can corroborate the
underruns. The counters print every 2 s while a client exists.
