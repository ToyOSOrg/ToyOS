---
status: open
kind: tooling
opened: 2026-10-02
---

# The guest test crate depends on three crates no test uses

`tests/toyos-rust-tests/Cargo.toml` names `blockd`, `toyos-blockring` and
`toyos-fat32`, "for `blockd_io`". `520c0d129` deleted
`tests/toyos-rust-tests/src/bin/blockd_io.rs`, and `git grep` for the three
under `tests/toyos-rust-tests` finds the manifest and its lockfile alone.

**Exit**: the three lines and their comment are gone, the lockfile follows, and
`cargo test --test toyos-build` is green.

Owner: the orchestrator.
