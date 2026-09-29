---
status: assigned
kind: track
opened: 2026-09-05
---

# The tooling is a review prompt and three workflows

The rules a prompt can read off a branch move into `.claude/agents/reviewer.md`,
and the gates that held them go.

- A test is green and fast or it is deleted in the same pull request and filed;
  then `src/redlist.rs`, `35383398^:src/durations.rs` and
  `tests/test-durations` have no subject and go.
- The toolchain is content-addressed by the four trees that produce it, one
  directory per hash, never mutated; then the sysroot claim, `src/buildlock.rs`
  and `src/worktree.rs` go.
- The nine workflows become three — `pr`, `nightly`, `publish`; then
  `a5b25a75^:src/mergehealth.rs` goes, and the ABI-lands-alone
  rule moves into the review prompt.
