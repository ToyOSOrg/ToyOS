---
status: open
kind: tooling
opened: 2026-09-30
---

# `toyos_tco::HARD_LOCKUP_BOUND_MS` is read by nothing but its own assertion

`toyos-tco/src/lib.rs` declares `HARD_LOCKUP_BOUND_MS`,
`hard_lockup_bound_ms(WEDGE_BOUND_MS)`, as "the one a T14 boot runs under",
and the `const _` beside it is its one reader. Its last other reader was the
metal profile's list of declared ceilings, which went with
`tests/metal-profile.toml`. The T14's `hardlockup` arm no longer runs under it
either: `metal::bound_for` arms that image with `toyos_tco::STAGED_BOUND_MS`, so its
lockup bound is `hard_lockup_bound_ms(STAGED_BOUND_MS)`.

`git grep HARD_LOCKUP_BOUND_MS -- ':!issues'` lists `toyos-tco/src/lib.rs`
alone.

## Owner

`issues/hardware/the-t14-boots-toyos-unattended.md`, the track that holds the
hard-lockup detector and its bound.

## What would close it

The constant and its assertion deleted, with whatever `toyos-tco`'s version
owes for it; `hard_lockup_bound_ms`'s own test already holds the half.
