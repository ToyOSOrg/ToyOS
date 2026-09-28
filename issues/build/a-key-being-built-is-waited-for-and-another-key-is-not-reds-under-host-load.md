---
status: open
kind: tooling
opened: 2026-09-28
---

# `a_key_being_built_is_waited_for_and_another_key_is_not` reds under host load

`src/buildlock.rs`'s last assertion, `keyed_idle(&root, Keyed::Sysroot,
"k1").is_some()` after the `using` guard drops, failed once under `cargo test
--lib` on a host running several other agents' builds concurrently
(`wt/toyos-desk1` at `e039fe6b`). Run alone straight after, the same test
passed in 0.59 s. The test spawns real child processes and times state
transitions against wall-clock sleeps and deadlines (`appeared`,
`keyed_idle`'s own polling), so host contention can move an event past a
window the test assumed was empty.

**Exit**: reproduce under synthetic host load (parallel `cargo build`s pinned
to the same cores) to find which wait the contention defeats, then widen that
wait or replace the polled read with the event itself.
