---
status: open
kind: tooling
opened: 2026-09-29
---

# Two build-system tests red once on a TempDir something was still writing into

One `cargo run -- --ci host` on `wt/toyos-phdr` (head `bf35e833`) reddened
"the build system: cargo test --lib" with two failures. In both, the test body
passed and `TempDir`'s drop (`toyos-tmpdir/src/lib.rs`, `fail`) panicked
because the directory was not empty when it removed it:

- `compiler::tests::llvm_and_the_tools_move_the_key`: `remove
  .../toyos-tmp-73906-0/compiler-key-33: Directory not empty (os error 66)`;
- `sysroot::tests::a_worktree_pinning_another_fork_commit_gets_its_own_checkout`:
  the same error on the process root `.../toyos-tmp-73906-0`.

Both tests drive `git` (commits, `update-index`, a worktree with a nested
submodule) inside the directory before it is removed. Something outside the
test body still wrote into it between the last entry `remove_dir_all` listed
and the final `rmdir`; what that is was not identified.

A plain `cargo test --lib` on the same tree right after exited 0 with 408
passed, so this is intermittent. The branch changes nothing under `src/` or
`toyos-tmpdir/`.
