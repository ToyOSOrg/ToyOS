---
status: open
kind: track
opened: 2026-10-08
---

# The fork's pin is a file, and a worktree checks no fork out

The `rust` gitlink becomes a pinned commit in a file, and a worktree that does
not edit the fork holds no checkout of it. The host's store
(`src/keystore.rs`) and every compiler in it came first; a worktree still
makes a fork checkout, 61,627 files and 399 MB as an export, to read its keys from, and still builds std in
it.

What is left, each step landing with `main` working:

- **The fork's objects are the store's.** A bare repository in the store
  holds the fork, `library/backtrace` and `src/llvm-project`, each fetched by
  commit, and every key's fork parts are git tree ids read from the pinned
  commit there, with no checkout. The pin is still the gitlink.
  *Exit*: a fresh worktree whose sources are `main`'s runs `cargo run --
  --build-only` with no `rust/` made in it and builds no compiler and no
  sysroot.
- **A pinned build reads an export.** Std, the compiler and LLVM are built
  from the pinned commit exported with no `.git`, beside copies of the
  worktree's `toyos-abi`, `toyos` and `sdk/std`, and bootstrap is given its
  source directory, so no build runs `git submodule`. A `rust/` that exists is
  the agent's fork work and is built as it stands; every build with one that is
  not the pin says so.
  *Exit*: `issues/the-fork-checkout-runs-git-submodule-in-a-linked-worktree.md`'s
  exit, and the sysroot such a build makes has the symbol tables of one built
  in a checkout.
- **The pin is a file.** Owner: "Yes to the pin file". The repository's URL
  and a 40-digit commit, in place of the gitlink and `.gitmodules`; `rust/` is
  ignored; `fork_checkout`, `ensure_submodule`, `ensure_shallow_fork` and
  `toolchain::Owner` go, and a runner keys and finds its sysroot as a dev host
  does. A full commit id counts as pinned by hash (orchestrator).
  *Exit*: the tree holds no gitlink, and root `CLAUDE.md` no longer forbids
  `git clone` or `git submodule` in a worktree.
- **A pin off the fork's `main` is refused.** The gate fetches the fork's
  `main` commits-only to a bounded depth, and can only say "within N of
  `main`" (orchestrator).
  *Exit*: `issues/seven-commits-of-mains-history-pin-a-rust-commit-no-repository-holds.md`'s.

Measured once each, on a host whose load average was 20 to 35, at fork
`cc9c8b1be68`:

- The fork by commit, trees only (`--depth=1 --filter=blob:none`): 2.09 MB in
  1.0 s, and every key's tree id is in it. With blobs: 49.3 MB in 12.1 s.
  LLVM with blobs is about 280 MB.
- A trees-only repository fetches a blob on a plain read: reading
  `.gitmodules` went to the network by itself. Either every git command on
  the store carries the request's environment, or it runs with
  `GIT_NO_LAZY_FETCH=1` and fetches blobs by id.
- The fork's `main` commits-only: 19.7 KB at depth 10, 13.6 MB at depth 50,
  83.1 MB at depth 200, since depth counts upstream's merge history too.
- Std built from an export with `--src` ran no git command, and its `libstd`
  rlib had the 1,758 defined symbols of the one built in a checkout, none
  differing in size, type or name.
- An export must carry no `git-commit-info` file, which makes bootstrap take
  it for a tarball.
