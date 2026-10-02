---
status: open
kind: tooling
opened: 2026-10-02
---

# A runner image that moves cc, c++ or CMake makes every toolchain job cold

`llvm::key` reads the `cc`, `c++` and CMake that build an LLVM, each by its
resolved path and all its `--version` says (`src/llvm.rs`), and
`toolchain.yml`'s `build` takes all three from `ubuntu-24.04`'s image
(20260927.320.1 in job 110610545360). An image that moves one moves every LLVM
key, and with it every compiler, freestanding and sysroot key. Every pull
request and merge group then builds all four layers until a run on main has
saved the new keys: a pull request's saves reach no other ref, and a merge
group saves only its sysroot.

That build is the cold path: 153:17 from the run's creation to `guest / suite`
green in run 36934214557, 2:09:01 of it `--ci bootstrap`, against the merge
queue's `check_response_timeout_minutes`.

Owner: the toolchain job (`.github/workflows/toolchain.yml`).

**Exit**: the tools an LLVM's key reads move only with a commit to this
repository, and a toolchain job on a runner image newer than that commit keys
the LLVM as the one before it did.
