---
status: expected-red
kind: tooling
opened: 2026-09-28
---

# A keyed lock read right after its own guard drops reds under host load

`src/buildlock.rs`'s `a_key_being_built_is_waited_for_and_another_key_is_not`
asserts `keyed_idle(&root, Keyed::Sysroot, "k1").is_some()` right after the
`using` guard for that same key drops; it failed once under `cargo test --lib`
on a host running several other agents' builds concurrently (`wt/toyos-desk1`
at `e039fe6b`), and twice more in a 200-run full-suite loop (runs 92 and 135),
always the same panic at that assertion. Run alone straight after, the same
test passed in 0.59 s.

`src/sysroot.rs:942`'s `a_sweep_removes_what_no_worktree_names_and_nobody_uses`
failed the same way in the same loop (run 134): `sweep(&root)` still counted
the `in-use` key as swept right after its `using` guard dropped. Same shape as
the assertion above — a keyed lock's on-disk state is read a beat before the
drop that should have freed it has actually landed — so it is the same
defect, not a second one.

Both tests spawn real child processes and time state transitions against
wall-clock sleeps and deadlines (`appeared`, `keyed_idle`'s own polling), so
host contention can move an event past a window the test assumed was empty.
Both are disabled
(`#[ignore = "issues/build/a-key-being-built-is-waited-for-and-another-key-is-not-reds-under-host-load.md"]`)
until this is fixed.

**Exit**: reproduce under synthetic host load (parallel `cargo build`s pinned
to the same cores) to find which wait the contention defeats, then replace
the polled read with the event itself; remove both `#[ignore]`s in the same
change.

## Owner

Held by the orchestrator.
