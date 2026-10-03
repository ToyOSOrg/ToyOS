---
status: open
kind: tooling
opened: 2026-10-03
---

# Two processes starting in one worktree are two movers of its fork checkout

`sysroot::make_fork_checkout` (`src/sysroot.rs`) makes a linked worktree's
`rust/`, or moves it to the commit the tree pins, once where a process starts,
so one process is one mover. Two build-system processes that start in one
worktree while its checkout is not made or is behind its pin are two, under no
lock: both run `git worktree add` or `git checkout --detach` there.

## Measured

In a worktree whose `rust/` was the empty stub, on a host at load average 36
of 14 cores, `cargo test --test toyos-build -- screen_panic_muted` and
`cargo test --test toyos-build -- screen_fatal_halt_composited` were started
together. Both printed `Making …/rust a fork checkout`. The first exited 101 on
git's `fatal: '…/rust' already exists`; the second made the checkout and exited
0.

## Owner

The toolchain item of
`issues/build/the-tooling-is-a-review-prompt-and-three-workflows.md`, which
makes the build system's locks unnecessary rather than writing them down.

**Exit**: two build-system processes started together in a new worktree both
exit 0.
