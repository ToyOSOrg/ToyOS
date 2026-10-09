---
status: open
kind: tooling
opened: 2026-10-08
---

# A compiler's build reads an environment and a cargo configuration its key does not name

Two inputs of `compiler::build_in_fork` are outside the fork checkout, and
`compiler::key` reads neither:

- **The caller's environment.** `toolchain::x_build_compiler` gives bootstrap
  the whole environment of the process that asked, less `GITHUB_ACTIONS` and
  `CI`, where the LLVM build gets `PATH` and `TMPDIR` alone (`llvm::clear`).
  `RUSTFLAGS`, `CC`, `CXX`, `CARGO_*` and `MACOSX_DEPLOYMENT_TARGET` reach the
  compiler's build; the C and C++ compilers a compiler's key names are the
  ones its LLVM's key resolved in the cleared environment, which a `CC` in the
  caller's makes another.
- **The worktree's cargo configuration.** Bootstrap runs cargo in
  `<worktree>/rust`, and cargo reads every `.cargo/config.toml` above its
  working directory: the worktree's, and through its `include` the untracked
  `.cargo/local.toml` in which `.claude/agents/implementer.md` has a fork clone
  under edit listed. The compiler's workspace patches four crates to ToyOS
  forks (`libloading`, `memmap2`, `stacker`, `getrandom`); a `local.toml`
  redirecting one to a local clone builds a compiler from that clone under the
  key of the one the lockfile names. The committed `config.toml` holds only
  guest-target tables today, which a host-only build does not read.

A compiler built under either is stored and served to every checkout naming
its key. By reading (`src/toolchain.rs`, cargo's documented configuration
search); no compiler was built under a differing environment or a
`local.toml` to measure it. Both are `main`'s shape since worktrees built
compilers of their own; the host's store widens who is served.

Owner: the step of
`issues/the-forks-pin-is-a-file-and-a-worktree-checks-no-fork-out.md` that
builds from an export ("A pinned build reads an export"), which decides where
a compiler's build runs; the orchestrator briefs it.

**Exit**: a compiler's build runs with the environment `llvm::clear` leaves
and in a directory no worktree's `.cargo` is above, or its key names what of
either it reads; and a host test in which a variable set in the caller's
environment does not reach the command the build runs.
