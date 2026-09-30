---
status: open
kind: tooling
opened: 2026-09-30
---

# The fork checkout runs `git submodule` in a linked worktree

`sysroot::shared` makes the host's shared fork checkout a linked worktree of
the primary's `rust` (`git worktree add --detach`), and a linked worktree's own
`rust/` is one too. `ensure_submodule` (`src/lib.rs`) runs
`git submodule update --init library/backtrace` in either, from
`sysroot::build_std` and `compiler::build_in_fork`, whenever its
`library/backtrace` is empty or gone, which `.claude/agents/implementer.md`
forbids. The orchestrator measured that for `git submodule update rust` in a
linked worktree of the monorepo, which set the primary's
`.git/modules/rust/config` `core.worktree` to a path that does not exist.

**Exit**: the build system runs no `git submodule` in a linked worktree's fork
checkout.
