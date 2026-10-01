---
status: open
kind: tooling
opened: 2026-10-01
---

# Bootstrap runs `git submodule` for the LLVM in a linked worktree's fork

`sysroot::fork_checkout` makes a linked worktree's `rust/` with `git worktree
add` of the primary's fork, which leaves `src/llvm-project` an empty directory.
When that worktree's LLVM key is in no store, `llvm::build_in_fork` runs
`x build src/llvm-project/llvm`. Bootstrap's `update_submodule` (fork
`aca5f527`, `src/bootstrap/src/core/config/config.rs:2585`) then goes on past
the empty directory. In it, `git rev-parse HEAD` answers the fork's own HEAD
rather than the gitlink, so bootstrap runs `git submodule -q sync` and
`git submodule update --init --recursive --depth=1 src/llvm-project` in the
linked worktree. That is the command
`issues/build/the-fork-checkout-runs-git-submodule-in-a-linked-worktree.md`
records for `library/backtrace`. This entry is read from the code and was not
run.

The way around it is to make `src/llvm-project` a `git worktree add --detach`
at the gitlink, from a clone that holds the commit. Bootstrap then returns at
`actual_hash == checked_out_hash`. This is how the LLVM gates of `toyos-libcxx`
and `toyos-non2` were run, by hand. If that clone is sparse
(`extensions.worktreeConfig`, with `core.sparseCheckout` in its
`config.worktree`), the new worktree inherits its patterns. `toyos-non2`'s
first build got only `clang`, `libcxx` and `libcxxabi`, and CMake stopped:
"The source directory .../src/llvm-project/llvm does not exist", exit 101.
Running `git sparse-checkout disable` in the new worktree alone fixed it.

**Exit**: a linked worktree whose LLVM key is in no store builds that LLVM
with no `git submodule` run in its fork checkout.
