---
status: open
kind: track
opened: 2026-08-03
---

# HDA plays, but there is no jack detection and nothing persists

The driver is the `hda` program's, one per function, through a `pci` claim:
the controller, codec decoding, output-path selection and the master are a
pure crate (`system/hda/driver`). The master volume and mute are the codec's,
set by the driver for soundserver's `volume` port, and the volume keys set them from the compositor
(#877). What remains:

- **Jack detection and routing.** `GET_PIN_SENSE` is spelled
  (`system/hda/driver/src/verb.rs`) and nothing issues it. Still wanted: a poll on the
  presence bit, a switch between the fixed speaker and the headphone pin, and a
  ramp across the switch so it is not a click.
- **Persistence** of the chosen output and the master level, blocked on the
  kernel keeping anything it enumerates (`issues/the-kernel-keeps-nothing-it-enumerates.md`).

**Two pieces of the shipped work are still owed a verdict.** The four HDA
sections of the audio baseline were never recorded, so the HDA arm asserts harm
and claims no distribution and has no thorough tier — that is 30 invocations on
a quiet host. And a shape profile is missing: a codec with no speaker pin (a
controller with no codec is refused by name in the controller's host tests). Without them the refusal-to-null-sink path is
asserted only on a machine that has no controller at all.

Constraints worth not re-deriving (each already stated at its site in
`system/hda/driver/`, listed here because they are what
the next pass will want):

- **The T14's trap is pin 0x1b, not the display codec.** Four pins claim
  "speaker" with no physical connection; only port connectivity separates them,
  and the display codec happens to sit at the higher address, so first-match
  wins there by luck.
- **EAPD is not optional** — both T14 output pins are EAPD-capable and read the
  bit clear at boot, so the obvious bring-up sequence plays silence.
- **Mute is at the pin, gain at the converter.** The T14 converter amp is 88
  steps of 0.75 dB with no mute bit; both pin amps are mute-only, one step.
- **The stream-format sample-base bit is 14, not 13.** `0x2011` is a 48 kHz base
  with a reserved multiplier that both controller and codec accept — and play
  8.8 % fast.
- **A cyclic HDA ring is not a free list**: an unrefilled period is replayed and
  completed twice, which is why the released buffer is zeroed.
