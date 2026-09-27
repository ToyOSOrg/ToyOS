---
status: assigned
kind: track
opened: 2026-09-27
---

# `toyos-elf` does not `forbid(clippy::arithmetic_side_effects)` yet

Held by the orchestrator's ELF-loader track; its next brief carries this.

The design's domain-lint line —
`#![forbid(clippy::arithmetic_side_effects, clippy::indexing_slicing, …)]` on
`toyos-elf/src/lib.rs` — is what would make an unchecked `+`/`-`/`[]` on a
file-chosen value a compile error, so the "every number is checked" property
this crate rests on is enforced rather than reviewed.

It is not added.

Exit condition: the line is on `lib.rs`, every site inside it is checked or
carries a one-clause `#[allow]`, and CI runs clippy on the crate with it.
