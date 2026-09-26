---
status: open
kind: tooling
opened: 2026-09-26
---

# `cargo package`/`cargo publish --dry-run` cannot run inside a linked worktree

`cargo package -p <any workspace member>` (tried `toyos-abi` and `toyos-ld`)
reds with a bare `error: No such file or directory (os error 2)` in a worktree
made by `cargo run -- --worktree add`, right after cargo's own trace logs
`found a git repo` and `found (git) Cargo.toml`, inside
`cargo::ops::cargo_package::vcs::check_repo_state` — before it prints anything
about a dirty tree, and regardless of `--allow-dirty` or a fully clean working
tree (confirmed with `git stash`). The identical command against the identical
crate exits 0 in the **primary** checkout. Every worktree carries dozens of
submodule entries (`userland/*`, `rust`) registered in the shared `.git/config`
but not checked out on disk (`rust`'s own empty stub included, per
`src/worktree.rs`) — a repo shape only a linked worktree has, and the likely
reason cargo's git-repo-state walk (`git2`) chokes there and not in the
primary.

Reproduce: from any `--worktree add` checkout, `cargo package -p toyos-abi
--no-verify --allow-dirty` exits 101 with that message; the same command in
the primary checkout exits 0.

Worked around for one branch's verification by copying the crate directory
alone (no path dependencies) outside any git repository and running `cargo
package`/`cargo publish --dry-run` there instead — not a fix, since it cannot
verify a crate whose own tree state (uncommitted changes, path deps) matters.
Nobody has yet bisected which of the two differences (submodule shape vs.
worktree-ness itself) is the actual cause, or whether a newer `cargo`/`git2`
carries a fix.
