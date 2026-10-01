---
status: open
kind: track
opened: 2026-10-01
---

# The workflow machinery becomes agent instructions

A rule a reader can check lives in an agent's prompt; the build system keeps
only what reading cannot see: runtime behaviour, bytes, measurements. Each
stage deletes machinery that does a prompt's job, or makes it unnecessary, and
lands green on `cargo run -- --ci host` and `cargo run -- --build-only`. Line
counts are `wc -l` at the base of stage 1.

1. **Making and removing a worktree and syncing the primary are root
   `CLAUDE.md`'s `git` commands**, and the sysroot and freestanding stores are
   swept at every placement, as the compiler and LLVM stores already were.
   Done when it lands.
2. **No lock of the build system's own.** `src/buildlock.rs` (1,063 lines) and
   `src/keystore.rs` (392). Two agents building at once cannot be told apart
   by prose, so the locks are made unnecessary rather than written down: every
   store is content-addressed, published by an atomic rename and never
   rewritten, and the build system `cargo clean`s no crate target, so cargo's
   own lock is the only one. A bullet of
   `issues/build/the-tooling-is-a-review-prompt-and-three-workflows.md` ends
   in the same deletion.
   Exit: both files are gone, and two builds started together in two
   worktrees are measured green.
3. **Every gate earns its place.** Each gate in `--ci host` and in the harness
   is judged keep (it sees what reading cannot), instruction (a reviewer can
   check it by reading, so it moves into a prompt) or delete, biggest first:
   `src/build.rs` (3,675), `src/metal.rs` (3,556), `src/licence.rs` (1,910),
   `src/metaltalk.rs` (1,720) and the harness (11,119 in `tests/toyos.rs`,
   `tests/checks.rs` and `tests/common/`).
   Exit: each one's verdict is applied.
4. **The instrumentation simplification the owner asked for.**
