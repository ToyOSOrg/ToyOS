---
status: open
kind: tooling
opened: 2026-09-16
---

# Nothing compares a committed tier row or profile row to the artifact the run measured

`.github/workflows/ci.yml`'s `durations` job merges the shards, grades the
merged profile against `src/tiers.rs`, and then asks
`git diff --quiet -- tests/test-durations` — and answers a difference with a
summary bullet ("`tests/test-durations` disagrees with what this run measured")
and exit 0. `Relegated::ci_ms` is documentation by its own doc, and
`Relegated::why` is read only to choose which rule grades the row. So a
committed number that is not what any run measured passes every gate a landing
runs, and a wrong `Why` on a clockless verdict passes them too.

Four partial fixes of the tier-sync landing that every gate it named stays
green under, each a mutation no test catches:

1. A `Why::Cost` row given `Why::TimerAnchored` instead — nothing reads the
   `Why` of a verdict with no clock in it, and `TimerAnchored` is never graded
   against a price in either direction.
2. `ci_ms` on any `RELEGATED` row replaced by any number — the field is never
   compared to the profile.
3. `audio_tone (smp=8)` and `audio_tone_load (smp=8)` in `tests/test-durations`
   hand-typed to any value over 8,000 — the committed profile is compared to
   nothing; the CI step writes a bullet and no red.
4. A `RidesTheBootOf` rider left `Tier::Nightly` with its row while its carrier
   returns — `cargo test -p toyos-build --lib` stays green; only the harness's
   own `check_registration` in `tests/toyos.rs` sees a group split across two
   tiers, and it runs only under `cargo test --test toyos-build`.

Site: the `merge durations` step of `.github/workflows/ci.yml`, `Relegated` in
`src/tiers.rs`, and the tier gates in `cargo test -p toyos-build --lib`.

**Exit condition**: each of the four mutations above reds a gate a pull request
runs — the committed profile refused when it is not byte-for-byte what the
instrument of record measured, or `ci_ms` refused when it is not the profile's
row, or `Why` graded against the verdict it claims, or the group rule run by
the lib suite — named in the landing that closes this as its negative controls.
