---
status: open
kind: track
opened: 2026-09-27
---

# `toyos-elf` does not `forbid(clippy::arithmetic_side_effects)` yet

The design's domain-lint line —
`#![forbid(clippy::arithmetic_side_effects, clippy::indexing_slicing, …)]` on
`toyos-elf/src/lib.rs` — is what would make an unchecked `+`/`-`/`[]` on a
file-chosen value a compile error, so the "every number is checked" property
this crate rests on is enforced rather than reviewed.

It is not added. About 100 existing sites in the crate use plain arithmetic and
indexing on values that are already bounded by other means, and each would need
either a checked form or a justified `#[allow]`. #544 wrote its new code
lint-clean but did not add the line or do the sweep; the design stages that as
E1.

Exit condition: the line is on `lib.rs`, every site inside it is checked or
carries a one-clause `#[allow]`, and CI runs clippy on the crate with it.
