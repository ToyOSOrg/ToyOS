---
status: open
kind: tooling
opened: 2026-09-28
---

# `record` reads `compiler/` as it stands after the bootstrap it records

`compiler::record` (`src/compiler.rs`) is called once `reassemble` has finished
building `stage2`, and it computes `source(rust_dir)` at that point — the
`compiler/` tree, working diff and untracked files as they read *then*, not as
they read when the bootstrap it is recording began. A `compiler/` edit made
while that bootstrap was running (`x.py build` takes minutes) is folded into
the record even though `stage2` was built without it, so the next build sees
`primary_is_current` return true for a `stage2` that does not contain the edit,
and skips a bootstrap that is actually owed.

Main has the same order (`record`'s only caller runs it after the build, and
main's predecessor — the `compiler.stamp` mtime plus the `moved` comparison —
had the identical property), so this is not a regression of this branch; it is
carried forward unchanged.

Exit condition: `record` (or whoever calls it) captures `source(rust_dir)`
before the bootstrap starts, not after, and a test edits `compiler/` mid-build
(a `bootstrap` closure that writes a file before returning) and asserts the
next `primary_is_current` is false.

Owner: whoever next touches `compiler::record` or `rebuild_compiler`.
