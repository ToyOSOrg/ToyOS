---
status: open
kind: tooling
opened: 2026-09-30
---

# The fork checkout runs `git submodule` in a linked worktree

`sysroot::fork_checkout` makes a worktree's `rust/` a linked worktree of the
primary's `rust` (`git worktree add --detach`). When the primary's
`library/backtrace` lacks the commit the fork pins, it runs
`git submodule update --init library/backtrace` inside that linked worktree,
which `.claude/agents/implementer.md` forbids: `git submodule` in a linked
worktree writes `core.worktree` into shared config and breaks git in the
primary checkout's `rust/`. The orchestrator measured that for
`git submodule update rust` in a linked worktree of the monorepo, which set the
primary's `.git/modules/rust/config` `core.worktree` to a path that does not
exist; this arm is the same command one level down and is unmeasured.

`ensure_submodule` (`src/lib.rs`) runs `git submodule update --init
library/backtrace` in the same fork checkout, from `sysroot::build_std` and
`compiler::build_in_fork`, whenever that checkout's `library/backtrace` is
empty or gone: what a first `fork_checkout` leaves when it stops after adding
the fork's worktree and before adding `library/backtrace`.

**Exit**: the build system runs no `git submodule` in a linked worktree's fork
checkout.
