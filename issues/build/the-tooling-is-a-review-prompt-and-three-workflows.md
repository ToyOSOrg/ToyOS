---
status: assigned
kind: track
opened: 2026-09-05
---

# The tooling is a review prompt and three workflows

Owner ruling: "i think we have to get rid of the prose gate and just give the
review agent a really good prompt especially against prose and comments ... we
need to move some stuff from code into better prompting. for example i think we
might even be able to get rid of the worktree code."

## What stays in code

A prompt reads the branch in front of it and cannot see, after the fact, what
the branch quietly failed to declare. So these are not moved into prompting:
`src/sourcegate.rs`'s `HOST_SPAWNS` and `COMMITTED_FILES` tables, which
enumerate a population and red on an arrival into it or a departure from it;
`src/kernelkeys.rs`; the QEMU pin in `.github/qemu-version` that `src/ci.rs`
refuses a guest-booting workflow for not naming; and the harness.

## The stages

1. `.claude/agents/reviewer.md` is the review prompt. Then the comment ratchet
   and its ledger, the prose-per-code law and its flag and CI step, the issue
   frontmatter and citation gate, and the actuator gate all go. Nothing of this
   stage has landed yet.
2. A test is green and fast, or it is deleted in the same pull request and
   filed. Then `src/redlist.rs`, `src/tiers.rs`, `src/durations.rs` and
   `tests/test-durations` have no subject left and go.
3. The toolchain becomes content-addressed by the four trees that produce it,
   one directory per hash, never mutated. Then the sysroot claim,
   `src/buildlock.rs` and `src/worktree.rs` lose their reason and go.
4. The nine workflows become three — `pr`, `nightly`, `publish`. Then
   `src/mergehealth.rs`, `landing.yml`'s `abi-split` job and its `gate-stage`
   job go, and the ABI-lands-alone rule moves into the review prompt.
