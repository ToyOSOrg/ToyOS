---
status: assigned
kind: track
opened: 2026-09-27
---

# `toyos::elf` does not `forbid(clippy::arithmetic_side_effects)` yet

The domain-lint line —
`#![forbid(clippy::arithmetic_side_effects, clippy::indexing_slicing, …)]` on
`sdk/toyos/src/elf/mod.rs` — is what would make an unchecked `+`/`-`/`[]` on
a file-chosen value a compile error, so the "every number is checked" property
this module rests on is enforced rather than reviewed.

It is not added.

Exit condition: the line is on the module, every site inside it is checked or
carries a one-clause `#[allow]`, and CI runs clippy on it.

Held by the orchestrator.
