---
status: assigned
kind: track
opened: 2026-09-05
---

# The tooling is a review prompt and three workflows

The rules a prompt can read off a branch move into `.claude/agents/reviewer.md`,
and the gates that held them go.

- A test is green and fast or it is deleted in the same pull request and filed.
- Each gate in `--ci host` and in the harness is kept where it sees what
  reading cannot (runtime behaviour, bytes, measurements), moved into a prompt
  where a reviewer can read it, or deleted. Exit: each one's verdict is applied.
- The toolchain is content-addressed by the four trees that produce it, one
  directory per hash, never mutated; then the sysroot claim, `src/buildlock.rs`
  and `src/keystore.rs` go. The locks are made unnecessary rather than written
  down: every store is published by an atomic rename and never rewritten, and
  the build system `cargo clean`s no crate target, so cargo's own lock is the
  only one.
  Exit: both files are gone, and two builds started together in two worktrees
  are measured green.
  The toolchain builds cargo from the Rust fork's submodule and ships it: one
  cargo matching rustc.
  - A shared cargo `target-dir` is safe only under `-Z checksum-freshness`;
    stable cargo ignores it silently.
  - The build system and the worktree config invoke the shipped cargo and no
    other.
  - `kernel/`, `bootloader/` and `userland/` inherit a redirected
    `build.target-dir` unless their `.cargo/config.toml` sets
    `target-dir = "target"`.
  - `stage_artifact` in `src/build.rs` builds its path outside
    `hostws::target_dir`.
  - `cargo clean` follows a shared target dir.
  - Artifact size, bootstrap delta and CI cache keys are unmeasured.
- The nine workflows become three — `pr`, `nightly`, `publish`; then
  `a5b25a75^:src/mergehealth.rs` goes, and the ABI-lands-alone
  rule moves into the review prompt.

**Ruled** (owner, 2026-10-04), on #629, which builds the content-addressed
toolchain item, **"Design pass, then re-cut"**: "A short design pass against
today's main (it would also ease the one-shared-fork-branch ordering
problem), roasted, then rebuilt as a fresh PR; #629 is closed in favour of
it."
