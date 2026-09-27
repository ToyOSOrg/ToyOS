---
status: open
kind: defect
opened: 2026-09-27
---

# The image release carries no notice of the crates its programs link

The image release carries ToyOS's own licences, `NOTICE`, and the texts
`NOTICE` names for the fonts and icons on the image (`LICENCE_ASSETS` in
`src/imagerelease.rs`). Every program on the image is also compiled from
third-party crates, the ones `src/licence.rs` walks, and MIT, BSD and
Apache-2.0 each ask that a copy of the software carry the licence text, and
MIT and BSD its copyright notice too. `NOTICE` says those crates keep their own
upstream licences, and nothing the release publishes carries one.

**Exit condition.** The release carries the licence text and copyright notice
of every package the licence gate finds in the release image, generated from
the gate's own walk, and a test fails when a shipped package has none.
