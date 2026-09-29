---
status: open
kind: tooling
opened: 2026-09-29
---

# `save_durations` publishes through a fixed `.tmp` and ignores every error

`save_durations` in `tests/toyos.rs` writes `path.with_extension("tmp")` and
renames it, discarding the result of `create_dir_all`, `write` and `rename`. Two
harness processes in one worktree share the one temp name, so one can rename the
other's half-written file into place; and a failed write leaves the profile stale
with no word said.

Exit condition: a temp name unique to the process, and an error that fails
loudly.
