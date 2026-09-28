---
status: open
kind: tooling
opened: 2026-09-28
---

# A dropped keyed lock is still held in the loaded lib suite

`buildlock::tests::a_key_being_built_is_waited_for_and_another_key_is_not`
went red once in 200 full `--lib` suite runs, at `8cc19ef1` and one-minute load average
40.1, beside `cargo test --workspace --exclude toyos-build`. It failed at
`src/buildlock.rs:945`: `assertion failed: keyed_idle(&root, Keyed::Sysroot,
"k1").is_some()`, right after `drop(using)`. Evidence:
`l3-staged-171.log` in the job scratchpad `redial-r3/`.

Unmeasured hypothesis: a `flock` belongs to the open file description, so a
child that another test in the same process is spawning shares `using`'s
lock until its exec closes the fd. `drop(using)` then releases nothing yet.
The same mechanism was measured for a dropped TCP listener, whose accepts
outlive its drop.

## Exit condition

The cause is measured, and the test's release is one no concurrent spawn can
defer. It is shown green across at least 200 full `--lib` suite runs beside
`cargo test --workspace --exclude toyos-build`.
