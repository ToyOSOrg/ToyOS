---
status: open
kind: tooling
opened: 2026-09-29
---

# A metal timing record never tightens

`src/metaltimings.rs`'s `judge` adds a name its machine has not recorded and
never moves one it has, so a run cannot loosen the record that judges it. The
same rule keeps a speed-up out: a number that halves keeps its old ceiling,
and a regression that later doubles it back is inside that ceiling and passes.
Only deleting the row re-records it.

## Exit condition

A recorded number that a run measures well under its record is either taken
as the new record by a rule that a noisy run cannot ratchet into a false red,
or reported by name so the row is deleted, and the choice is in
`src/metaltimings.rs`'s module doc.
