---
status: open
kind: tooling
opened: 2026-10-04
---

# Seven commits of `main`'s history pin a `rust` commit no repository holds

`96441690a` set the `rust` gitlink to `0e27504731a5f6a2f7c9d43e9e40e6b28b56a0e5`,
and seven commits of `main`'s history carry it, from `96441690a` to `7810c9f66`
on `wt/toyos-endow`'s side of its merge `44bc5acce`. `4670353eb` moved the pin
to `d91d5a423`. Those seven commits cannot be built: the toolchain their tree
names is not on the fork.

The pin was never pushed. `4670353eb`'s message says the fork head then was
`0e27504731a51efe`, which shares twelve hex characters with the pin, so every
short-hash check that branch ran took the one for the other. No force-push lost
it.

## Measured

At `42e5fca73`:

- `git ls-tree 96441690a rust` prints
  `0e27504731a5f6a2f7c9d43e9e40e6b28b56a0e5`.
- `git rev-parse "$c:rust"` for every `c` in `git rev-list origin/main` prints
  that commit for seven: `96441690a`, `4fc34a222`, `904353fb3`, `f34bf3460`,
  `1efa027cd`, `bc2a814cd`, `7810c9f66`. None is on `main`'s first-parent line.
- `gh api repos/ToyOSOrg/rust/commits/0e27504731a5f6a2f7c9d43e9e40e6b28b56a0e5`
  answers HTTP 422, `No commit found for SHA`.
- In the primary checkout's `rust/`, after `git fetch origin`,
  `git cat-file -t 0e27504731a5f6a2f7c9d43e9e40e6b28b56a0e5` exits 128, and
  `git rev-parse 0e2750473` prints `0e27504731a51efe39976648db289afadfdb2fbe`:
  a nine-character short hash names the other commit.

## Read, not measured

No check sees whether a commit a branch carries pins a `rust` commit the fork
holds. A worktree's build fetches its pin from the primary checkout's `rust/`
(`sysroot::fork_checkout`), never from the fork, so a commit made there and
never pushed builds green.

## Owner

Unassigned: the orchestrator, which lands every pull request that moves the
`rust` gitlink, assigns it.

**Exit**: a pull request any of whose commits pins a `rust` commit no fork
branch holds is refused before it lands. The seven commits above stay
unbuildable; the closing commit records them.
