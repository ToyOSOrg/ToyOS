---
status: open
kind: tooling
opened: 2026-10-10
---

# CI runs the whole guest suite

The owner's ruling: guest tests run on demand, and CI's small tier holds only the tests that must
break when something important breaks. `guest.yml` runs `cargo run -- --ci guest` (`src/ci.rs`,
`guest`), which is every registration in `tests/toyos.rs`, on every ready pull request, in the
merge queue (`ci.yml`) and nightly (`nightly.yml`); no tier is declared, so no review can keep a
new guest test out of it.

Which tests belong in the tier is the owner's call. Owner: the orchestrator, which dispatches the
reshape once he has named them.

**Exit condition**: `--ci guest` runs a declared list in `tests/toyos.rs` and nothing else, the
rest of the suite runs on demand, and root `CLAUDE.md`'s "The guest suite is their `guest /
suite` check" and `reviewer.md`'s **Guest tests** say so in the same diff.
