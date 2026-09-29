---
status: open
kind: tooling
opened: 2026-09-29
---

# `--worktree remove` leaves the local `wt/<name>` branch behind, so the name is refused later

`remove` unregisters and deletes the worktree directory but never deletes the
local branch (`src/worktree.rs:391`, the `eprintln!` that says so). If nothing
was ever committed on it, the branch is an ancestor of `origin/main` and
`add`'s `refuse_if_no_commit_beyond_main` (`src/worktree.rs:162`) later refuses
the same name — correctly, since it truly carries no commit beyond
`origin/main`, but the caller still has to notice and run `git branch -d
wt/<name>` by hand before the name is usable again.

**Exit**: `remove` deletes the local branch itself when it carries no commit
beyond `origin/main` (the same check `add` uses), so a name that was never
used becomes free again without a manual step.
