---
status: open
kind: tooling
opened: 2026-09-29
---

# A pipe made on macOS can leak into a sibling's spawn and red the tether tests

`src/tether.rs`'s `Owner` judges its tethered children by the end of the
owner's stderr pipe: the end is every holder exited. On macOS, std makes that
pipe with `pipe()` and marks it close-on-exec afterwards
(`library/std/src/sys/pipe/unix.rs`: `pipe2` is only used on the targets that
have it, and macOS does not). A process another thread of the same test binary
spawns in that window inherits the write end and holds it until it exits.

Both arms run beside sibling spawns: `a_tethered_child_dies_with_its_owner`
in `cargo test -p toyos-build --lib`, whose other tests spawn processes, and
`guest_dies_with_its_harness`, a `Sched::Parallel` test in the harness, beside
compiles and other guests' QEMUs. Such a leak turns a working tether into a
red: the pipe does not end within `WITHIN`, the owner's group is killed, and
the refusal says a holder outside that group still ran.
`toyos-tmpdir/tests/reclaim.rs` serialises its own spawns (`SPAWNING`) against
exactly this, and nothing here does.

Owner: whoever next changes `src/tether.rs`. Exit condition: the verdict no
longer depends on the end of a pipe made on macOS without close-on-exec — the
pipe made atomically close-on-exec, or every spawn in each process that runs an
`Owner` serialised against its making, or a verdict read off something other
than a pipe's end.
