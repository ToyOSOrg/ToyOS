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
`initializing`. `ensure_shallow_fork` refuses it by name. The licence gate
reaches that refusal only for a kill before `library/Cargo.toml` is written,
a file this fixture's fork never has: `licence::std_library` calls
`ensure_shallow_fork` only without it, and past it the gate reads what
`library/` holds and runs no `git submodule`.

In a fixture fork of 30,004 files the same kill left 253 of them,
`library/Cargo.toml` among them. Afterwards `git worktree add` at that path
exits 128 on `already exists`, `git worktree remove --force` exits 128 on the
lock, and `git worktree remove --force --force` exits 255 on
`Directory not empty` and unregisters the worktree.

## Read, not measured

Both measurements kill git. The kill `src/buildlock.rs`'s header calls routine
is the builder's alone. Then the kernel frees the worktree's lock at once, and
`git worktree add` goes on writing: it is the builder's child and holds no
descriptor of the lock, `git_run` being `Command::status` and the lock file
opened close-on-exec. A build that starts then finds `rust/.git`, `HEAD` at the
pin and the lock free. When that git ends, the checkout has no
`library/backtrace`, the state
`issues/build/the-fork-checkout-runs-git-submodule-in-a-linked-worktree.md`
records of a `fork_checkout` that stopped before adding it.

## Owner

The toolchain item of
`issues/build/the-tooling-is-a-review-prompt-and-three-workflows.md`, whose
stores are published by an atomic rename.

**Exit**: a build that finds a fork checkout git records as `locked` makes it
again or refuses it by name.
