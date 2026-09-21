---
status: open
kind: tooling
opened: 2026-09-21
---

# `redlist::tests::every_row_can_say_what_it_claims` reds on the calendar, on `main` and on every branch cut from it

`cargo test -p toyos-build --lib` on 2026-09-21 is 307 passed, 1 failed: four
`KNOWN_RED` rows measured 2026-08-20 and 2026-08-21 are "more than 31 days ago,
and still standing" — `console_line_atomicity`, `kill_while_blocked` (two rows)
and `xhci_full_speed_device`. The gate reads `Day::today()`, so nobody's diff
turned it red: `src/redlist.rs` on the branch that met it
(`wt/toyos-blockeddump`) is identical to `origin/main` at `9f91b581`, and the
same command was green (308 passed) at that branch's previous head, over the same file.

The gate is doing what it was written to do. What is owed is its answer, per
row: re-take the measurement, retire the row with what retired it, or delete it.
Until then the PR gate's `-p toyos-build --lib` step is red for every author.

Closed when the four rows are re-measured, retired or deleted and the command is
green on `main`.
