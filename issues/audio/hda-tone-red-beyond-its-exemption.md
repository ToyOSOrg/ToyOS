---
status: open
kind: defect
opened: 2026-08-07
task: 88
---

# `hda_tone` is red on `main` for a reason `#88`'s exemption does not cover

`cargo test -- hda_tone` on `main` at `6d11938`, alone, 2026-08-07 18:5x:

```
FAIL hda_tone: 1 mid-tone silences in the capture: total 1 [1p×1]
  FAIL  hda_tone  (15s)  — listed against #88, and this is not that failure:
        the entry covers ["the captured tone is not one sine"]
```

The `EXPECTED_FAILURES` entry does what it is supposed to: it pins the assertion
rather than the test, so a *second* defect in the same test still reds the run
and says which. What is red is the mid-tone-silence assertion — a gap in the
capture, which is gate A's harm verdict — and not #88's spectral one. The entry
still says so at the site: it names only `"the captured tone is not one sine"`,
beside a comment listing "no mid-tone silence" among the things that red the run
"because each of those is the milestone rather than the open question".

**What has changed since, and what has not.** Two things narrow the harm this
was filed for. `hda_tone` is `Tier::Nightly` for `Why::TimerAnchored`
(`src/tiers.rs`), so a plain `cargo test` no longer runs it and a landing whose
gate is `cargo test` no longer meets this red at all; and `src/redlist.rs`
carries two rows for the name — one `Seen` on the dev host alone, sourced here,
and one at 4 of 5 on CI — so an agent who does meet it is told whose it is
rather than reading it as theirs.

Neither touches the verdict, and the verdict is what is owed. Every capture
behind it was taken through QEMU's 48000→44100 resampler, which is no longer in
the path, so this assertion and #88's both want re-judging on a fresh sample
rather than carrying forward — `issues/audio/hda-verdicts-rest-on-pre-fix-captures.md`
is where that is tracked. A capture taken beside other guests is not that
sample.

Found while landing task #98/#12: the same test failed identically inside that
landing's gate, and the A/B against `main` in the same session is what
identified it as `main`'s. Assigning it needs whoever owns H3 —
`5fdfeb7`/`a022811` ("wip: H3, the virtio-sound stub and its userland driver")
landed hours before this measurement.
