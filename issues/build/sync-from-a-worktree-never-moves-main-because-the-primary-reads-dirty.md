---
status: open
kind: tooling
opened: 2026-09-29
---

# `--sync` run from a worktree never moves main, because the primary always reads dirty

`sync` skips the fast-forward when the primary checkout has uncommitted work and
the asking checkout is not the primary. The primary's `git status --porcelain`
always reads ` M rust`, so from any worktree `main` is left where it is and
only a `--sync` run in the primary itself moves it.

True on `origin/main` before the `--pr` removal too. Not fixed by the branch
that renamed `pr.rs` to `sync.rs`.
