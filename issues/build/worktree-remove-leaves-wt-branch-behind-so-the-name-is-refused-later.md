---
status: open
kind: tooling
opened: 2026-09-29
---

# `git worktree remove` leaves the local `wt/<name>` branch behind, so the name is refused later

`git worktree remove <path>` unregisters and deletes the worktree and keeps
its branch, and `git worktree add --no-track -b wt/<name> <path> origin/main`,
the command `CLAUDE.md` gives, then refuses the name: `fatal: a branch named
'wt/<name>' already exists`, exit 255. Somebody has to run `git branch -d
wt/<name>` by hand before the name is usable again.

**Exit**: a name whose branch carries no commit beyond `origin/main` is usable
again after `git worktree remove` without a manual step.
