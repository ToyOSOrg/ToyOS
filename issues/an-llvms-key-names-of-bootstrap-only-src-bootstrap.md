---
status: open
kind: tooling
opened: 2026-10-08
---

# An LLVM's key names, of bootstrap, only `src/bootstrap`

`llvm::key` reads the fork's bootstrap as the committed tree of
`src/bootstrap`. The bootstrap that builds an LLVM is also `src/build_helper`,
its path dependency (`src/bootstrap/Cargo.toml`), and the launchers
`toolchain::x_build_with` runs, `x` and `x.py`; a fork commit that moves only
one of them keeps the stored LLVM. A compiler's key reads all three
(`compiler::KEYED`); the LLVM's was left as it is because naming them moves
every LLVM key, which is one cold LLVM on each host (15:19 on an idle
development host, measured once) and three on CI.

Read in the fork at `6d6ad8c7190`, not measured: no LLVM was built from a
fork whose `src/build_helper` differed.

Owner: the first step of
`issues/the-forks-pin-is-a-file-and-a-worktree-checks-no-fork-out.md` ("The
fork's objects are the store's"), which reads every key's fork parts as git
tree ids and so moves every LLVM key anyway; the orchestrator briefs it.

**Exit**: `llvm::key` names `src/build_helper`, `x` and `x.py` as it names
`src/bootstrap`, refused while one holds what no commit does, and a test in
which a commit moving each alone moves the key.
