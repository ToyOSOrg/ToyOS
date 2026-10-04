---
status: open
kind: tooling
opened: 2026-09-29
---

# The primary's compiler record is written by truncate-then-write

`record` in `src/compiler.rs` writes `build/toyos-compiler` with `fs::write`,
which truncates and then writes. A linked worktree reads that file in
`primary_is` without the primary's lock, so a read between the two sees an empty
or partial record. It compares unequal to what the worktree's source names, and
the worktree builds a compiler of its own that the primary already has.

Exit condition: the record is written whole or not at all (a temp beside it,
renamed over), as `keystore::record` does for its records.
