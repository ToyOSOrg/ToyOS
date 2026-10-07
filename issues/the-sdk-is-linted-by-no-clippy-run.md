---
status: open
kind: tooling
opened: 2026-10-07
---

# The SDK is linted by no clippy run

`toyos` is a workspace member that no shape in `src/clippy.rs` builds:
`src/hostws.rs` counts it among the members built for a guest, so the
workspace shapes exclude it, and it has no shape of its own. `--ci host`
tests it and lints none of it.

`cargo clippy -p toyos --all-targets -- -D warnings` on the host exits 101
with 12 findings: `new_without_default` on `fs::Request`,
`manual_range_contains`, `len_without_is_empty` on `IpcHeader`, three
`chunks_exact` with a constant size, four manual `is_multiple_of`, the
disallowed `core::mem::forget`, and one byte-string literal in a test.

Each fix edits `toyos/src`, which is one of a sysroot's sources.

Exit: a shape lints `toyos` for the host, and it is clean.
