---
status: open
kind: tooling
opened: 2026-09-29
---

# No test covers the pid in a store record's temp name

`record` in `src/store.rs` names its temp file
`<record>.<pid>-<counter>.new` so that concurrent writers of one record never
share one. The counter distinguishes threads of one process; the pid is what
distinguishes two processes in one worktree, and nothing tests it: dropping
`std::process::id()` from the format leaves `cargo test --lib store::` green
(exit 0, measured by that mutation and that command)
while two processes writing the same record share a temp name again.

Exit condition: a test arm that races a child process against the parent on one
record, using the re-exec pattern of `rerun` in `src/dirlock.rs`, and goes red
when the pid is dropped from the format.
