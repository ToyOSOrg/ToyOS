---
status: open
kind: defect
opened: 2026-08-29
---

# `89_nocode_wanted` builds and runs green, and only its price keeps it off the suite

The statement-expression defect that stopped it is fixed: `compile_stmt_expr`
takes the construct's value from its *final* item with labels unwrapped, so
`kb_wait_3`'s dead-arm `goto` no longer hands the merge a value from a
non-dominating block. Run once on the dev host with a dev-time `UNMEASURED`
marker (2026-08-29): `PASS 89_nocode_wanted (33ms)`, output matching its
`.expect` exactly.

What is left is only the registration dance `tests/CLAUDE.md` prices: a new
name lands with an `UNMEASURED` row in `tests/test-durations`, that commit
stays red until the bought KVM run's artifact prices it, and the follow-up
commit assigns the final tier. A batch that may not leave a red behind cannot
carry that, so `NOT_RUN` holds the case at `Stage::Built` pointing here.

Whoever next pays the two CI cycles: delete the `89_nocode_wanted` entry from
`NOT_RUN` in `tests/toyos.rs`, commit the `UNMEASURED` row, and replace it
with the measured value from that run's `test-durations-merged` artifact. At
33 ms on a shared boot it is nowhere near any line.
