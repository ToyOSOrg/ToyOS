---
status: open
kind: tooling
opened: 2026-10-01
---

# A compiler key reads no symbolic link

`compiler::key` reads `compiler/`, `src/tools/`, `src/stage0` and
`Cargo.lock` through `sysroot::tree_identity` with `Links::Skipped`, so a
symbolic link there is in no key: retargeting one, or editing what it names
outside those trees, keeps the old compiler. Refusing a link there, as the
freestanding key does, refuses every compiler build:
`git -C rust ls-files -s compiler src/tools src/stage0 Cargo.lock` at fork
commit `aca5f527f` lists 5 entries of mode `120000`, all under `src/tools`
(clippy's and rust-analyzer's `LICENSE-APACHE` and `LICENSE-MIT`, and
rust-analyzer's `AGENTS.md`). Hashing a link's target text instead moves the
key of every compiler of a worktree's own once.

**Exit**: `Links::Skipped` is deleted and the compiler key hashes a link's
target text, landed with the next change to `compiler.rs`'s `RECIPE`, which
moves every compiler key anyway.
