---
status: open
kind: defect
opened: 2026-09-04
---

# `doom_sound_flood` played full scale once, and the volume that reached the wire was not the one the last command named

Nightly `ci` run `33728852421`, job `100563991283` (`guest (11)`), 2026-09-03,
KVM, one guest per machine:

```
FAIL doom_sound_flood: the device played a peak of 32768 (expected 4000..=12000): the volume the last command named is not the volume that reached the wire
  FAIL  doom_sound_flood  (7s)
```

The re-run alone in the same job was green and printed what a healthy run
prints:

```
  [doomcase] 4096 commands issued with the callback parked, tone converged in 173 periods for 22050 frames, 7963136 concurrent commands, 21160 samples of signal at peak 7969
  PASS  doom_sound_flood  (8s)
  ALONE doom_sound_flood: GREEN, and it was alone both times — nothing the harness controls differed, so it failed once and passed once. That is a rate and not a classification.
```

**Why the number matters.** `tests/common/audio.rs` derives the band from the
two outcomes the actuator can produce: 16000 × 127/255 = 7968 if the last
volume command is the one applied, and 251 if every superseded update is. 32768
is neither. It is `-i16::MIN` — the magnitude of the most negative sample an
`i16` holds — so the capture carried a full-scale sample, eight times the
amplitude the mixer was asked for and above anything a superseded command
explains. The alone re-run's `peak 7969` is the expected outcome to one count.

**What is not known.** Whether the full-scale sample is one sample or a span,
whether it is the mixer's sum overflowing or the analysis reading a wrapped
value, and whether a listener would hear it. Nothing in the captured line
distinguishes those, and the WAV that would is not kept by a CI job.

**Exit condition.** A run that reproduces the peak with the capture retained,
and one sentence naming which of the three it is. `src/redlist.rs` carries the
rate (1 of the 3 nightlies of that week); the two rows for this name that were
about `timed out after 88s` are retired on the change of shape.
