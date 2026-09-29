---
status: assigned
kind: track
opened: 2026-09-05
---

# The tooling is a review prompt and three workflows

The rules a prompt can read off a branch move into `.claude/agents/reviewer.md`,
and the gates that held them go.

- A test is green and fast or it is deleted in the same pull request and filed;
  then `src/redlist.rs`, `src/tiers.rs`, `35383398^:src/durations.rs` and
  `tests/test-durations` have no subject and go.
- The toolchain builds cargo from the Rust fork's submodule and ships it: one
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
