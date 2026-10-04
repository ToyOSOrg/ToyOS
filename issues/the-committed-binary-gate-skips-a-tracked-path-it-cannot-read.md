---
status: open
kind: tooling
opened: 2026-10-03
---

# The committed-binary gate skips a tracked path it cannot read

`src/sourcegate.rs`'s `every_committed_binary_file_is_declared` takes every
path `git ls-files` names and skips one that will not read, at `:747`:
`let Ok(bytes) = std::fs::read(root.join(name)) else { continue }`. Its claim
is that every committed file somebody had to judge is judged, and a path it
did not open is judged by nothing and named by nothing.

The one tracked path that does not read today is `rust`, the fork's gitlink,
a commit and not a file. `no_tracked_file_identifies_a_machine_or_its_network`,
in the same file, skips it by name and panics on any other.

**Evidence.** With `issues/xhci-waits-are-spins.md` moved out of the
worktree, `cargo test --lib every_committed_binary_file_is_declared` exits 0.

A declared file that stops reading still reds, as a stale row. What passes is
an undeclared path; by `std::fs::read`'s contract and not measured, a second
gitlink or a symbolic link to nothing.

`:216` and `:720` skip the same way, over a walk of the directory and not over
`git`'s list.

## Exit condition

The gate skips `rust` by name and panics on any other path that will not read,
and the command above exits 101.
