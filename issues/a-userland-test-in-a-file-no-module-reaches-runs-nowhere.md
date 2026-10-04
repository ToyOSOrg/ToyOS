---
status: open
kind: tooling
opened: 2026-09-26
---

# A userland test in a `src/` file no module reaches runs nowhere and still reads as gated

`src/userlandhost.rs` gates a userland crate by reading its `src/` and `tests/`
for test attributes. A file under `src/` that no `mod` declaration reaches is
never compiled, so a test in it runs in no `cargo test`, and the survey
counts its crate as gated all the same. The merge gate is green over a test
that never ran. No such file is in the tree today, and nothing refuses one.

The same holds for any file the survey reads that a target's module tree does
not include. A `#[path]` pointing away from the file, or a module dropped from
its parent, orphans it.

**Exit:** every file the survey finds holding a test is shown to be compiled
into a test binary the gate runs. For example, `host` lists each gated crate's
tests (`cargo test ... -- --list`), and each such file contributes at least one
listed test. A test fixture holding an orphaned `src/` file must red.
