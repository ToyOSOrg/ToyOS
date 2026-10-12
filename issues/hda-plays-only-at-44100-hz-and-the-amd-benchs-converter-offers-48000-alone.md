---
status: open
kind: defect
opened: 2026-10-12
---

# HDA plays only at 44100 Hz, and the AMD bench's speaker converter offers 48000 Hz alone

`lib/hda/src/config.rs` asks every converter for one rate, `RATE = 44_100`. soundserver refuses a converter that does not offer it (`Refusal::Rate` in `system/soundserver/src/hda.rs`) and presents the null sink.

**The AMD bench's speaker path** is pin 0x14 straight to converter 0x02:

- the converter's Format Override is set, so its own PCM word is the one that holds: `0x000e0040`, 48 kHz alone, at 16, 20 and 24 bits;
- its function group's word, `0x000e0560`, offers 44.1 kHz, but that word does not apply to a converter that overrides the format;
- the T14's converter reads `0x000e0060`: 44.1 and 48 kHz.

That was read off a boot of the bench's demo build. On `main` the bench never reaches its converter, because `issues/the-amd-bench-answers-on-two-hda-controllers-and-plays-on-the-null-sink.md` refuses both controllers first.

**The owner's ruling (2026-10-12):** native rate per chip. 44.1 kHz wherever the converter offers it, so the T14 is unchanged; a converter offering only 48 kHz runs at 48 kHz with high-quality resampling.

**Two more things stand on 44.1 kHz:**

- **cpal.** cpal's ToyOS backend advertises 44.1 kHz alone and asserts that soundserver's device rate equals it (`issues/cpal-backend-hardcodes-the-format.md`). A device at 48 kHz therefore aborts every cpal client. virtio-sound already runs at 48 kHz where a device offers no 44.1 kHz.
- **The resampler.** soundserver's resampler carries a client whose rate is not the device's, and no test holds its quality. Measured on the host against the analytic sine, with soundserver's parameters (#877, issuecomment-6120534796):
  - in-band THD+N is at least 135 dB below the tone;
  - the gain at 20 kHz is −0.31 dB from 44.1 to 48 kHz, and −0.43 dB from 48 to 44.1 kHz;
  - a 25 kHz tone resampled from 96 kHz leaves a −31.5 dB alias at 23 kHz.

Owner: soundserver's HDA driver (`system/soundserver/src/hda.rs`) and `lib/hda/src/config.rs`. The design is #877's A5.

## Exit condition

All of:

- an AMD bench boot reads soundserver on HDA at 48000 Hz, with its audio rows green;
- a T14 boot reads 44100 Hz, with its audio rows green;
- a cpal client takes the rate soundserver answers;
- a host test holds soundserver's resampler to a bound derived from the 16-bit wire.
