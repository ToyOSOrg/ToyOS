---
status: open
kind: defect
opened: 2026-08-22
---

# An actuator's doc can name its reader and nothing checks that the reader is registered

`kernel/src/actuator.rs`'s macro header says the comment on each actuator "is the
claim that earns it a place". Several of those comments end by naming the test
that reads the actuator — `log-unbracketed-reserve` named `log_migration_storm`,
`no-ap-control-regs` names `control_regs_negative`, `nvme-spent-budget` names
`cache_eviction`. One of those three named a test that did not exist, for as long
as the actuator did, and the only thing that found it was somebody reading the
file.

Both halves of the check are already in the tree and neither is joined to the
other: `tests/toyos.rs`'s registration table is the set of names a boot can be,
and `src/redlist.rs` already resolves doc paths out of Rust source, so reading a
backticked identifier out of an actuator's doc comment and asking whether the
table has it is the same shape of gate.

The narrow version is enough and is what makes it a gate rather than a style
rule: an actuator doc naming an identifier that *looks like a test name* — snake
case, no `::`, not a Rust item this crate declares — must name one the
registration table carries. Everything else in the comment stays prose.

What this does not catch, and what would still be owed after it: an actuator
whose named reader exists, is registered, boots it, and asserts nothing that the
actuator changes. `log-unbracketed-reserve` was that too — `log_conservation_smp8`
was the only name that plausibly read it and stayed green 4 of 4 with the bracket
removed. A gate for *that* is a different and much harder thing, and the honest
substitute is the negative-control arm: a registered test that arms the actuator
and asserts the failure by name, which is what `log_reserve_window_negative` now
is.

**2026-08-25: promoted.** The narrow gate described above still does not exist
— nothing in `src/redlist.rs` or `src/build.rs` cross-checks a backticked
identifier in an actuator's doc comment against `tests/toyos.rs`'s registration
table, verified against the current tree. Whoever next touches
`kernel/src/actuator.rs`'s macro header or `src/redlist.rs`'s doc-path
resolution should build it; `src/build.rs`'s `declared_actuators` is the
closest existing parser and the likely place to add the registration-table
cross-check.
