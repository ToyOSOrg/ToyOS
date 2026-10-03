---
status: open
kind: tooling
opened: 2026-10-03
---

# A fork checkout a killed `git worktree add` left is taken as made

`sysroot::fork_checkout` (`src/sysroot.rs`) takes a linked worktree's `rust/` as
made where `rust/.git` exists and `HEAD` is the pinned commit or ahead of it. A
`git worktree add` killed while it writes its files leaves both, so every build
after it is handed a checkout with files missing. Git records the state itself:
the worktree stays `locked` with the reason `initializing`, beside a stale
`index.lock`.

## Measured

In a fixture whose fork checks its last file, `x.py`, out through a filter that
waits, `git worktree add --detach` was killed with SIGKILL once `rust/.git`,
`HEAD` and the files before `x.py` were written. `fork_checkout` returned what
it left: `rust/.git`, `HEAD` at the pin, no `x.py`, `locked` reading
`initializing`. `ensure_shallow_fork` refuses it by name, so the licence gate
reds on it.

In a fixture fork of 30,004 files the same kill left 253 of them. Afterwards
`git worktree add` at that path exits 128 on `already exists`,
`git worktree remove --force` exits 128 on the lock, and
`git worktree remove --force --force` exits 255 on `Directory not empty` and
unregisters the worktree.

## Owner

The toolchain item of
`issues/build/the-tooling-is-a-review-prompt-and-three-workflows.md`, whose
stores are published by an atomic rename.

**Exit**: a build that finds a fork checkout git records as `locked` makes it
again or refuses it by name.
