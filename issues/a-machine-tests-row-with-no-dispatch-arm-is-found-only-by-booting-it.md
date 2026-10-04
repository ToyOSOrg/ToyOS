---
status: open
kind: tooling
opened: 2026-09-28
---

# A `MACHINE_TESTS` (or `SCREEN_TESTS`) row with no matching arm in `run_machine_test` / `run_screen_test` compiles, lists and clips clean

`run_machine_test`'s and `run_screen_test`'s final arm is a catch-all —
`other => Err(format!("unknown input test {other}"))` — so a name registered
in `MACHINE_TESTS` or `SCREEN_TESTS` with no corresponding `match` arm is not a
compile error: it is a `String` that falls through to that arm the one time
something schedules it. `cargo test --test toyos-build -- --list`, `cargo run
-- --clippy` and CI `host` never schedule a machine test, so none of them call
it, and `65511a24` (a partial revert left by a merge's conflict resolution
restoring `i8042_health_cadence`'s registration alone) passed all three at
`6c8ff3f4`; the gap surfaced only when a nightly boot actually ran the name and
got "unknown input test i8042_health_cadence" back.

## Exit condition

A host check — run in `host` or `--list`, not only on a boot — that fails on
an orphan either way: a registration with no arm, or an arm with no
registration. `check_metal_registration`'s `metal_rows_are_registered`
already does this comparison for `METAL`/`METAL_ONLY` against
`MACHINE_TESTS`/`SCREEN_TESTS`; the same shape of check, run against every
`match` arm's own pattern literal instead of a second table, closes this.

## Owner

`tests/toyos.rs`, `run_machine_test` and `run_screen_test`. Nobody holds it.
