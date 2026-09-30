---
status: open
kind: tooling
opened: 2026-09-29
---

# The toolchain store shares its host with the layout it replaced

Until every worktree on a host has merged the store (`src/store.rs`), two
layouts build into one `rust/build/`, and neither sees the other's holds:

- A build on the old layout runs `keystore::sweep`, which removes every key no
  worktree records and every `<key>.*` name in a store directory. A store key
  that a build holds by `flock` and has not yet recorded is in reach of it.
- `store::collect` sees neither the old layout's `buildlock` holds nor its
  `.making` claims, so a key an old-layout build is using and no worktree
  records is in reach of it.
- Every worktree `rust/` the old layout made carries `library/backtrace` as a
  git worktree of the primary's clone (20 on this host, `git worktree list`
  there). An old-layout build in one whose backtrace is at any commit but its
  gitlink — after a gitlink bump, or after the checkout moved, which leaves its
  submodules where they were — has bootstrap run `git submodule update` over it
  (`update_submodule` in the fork's `src/bootstrap/src/core/config/config.rs`),
  which rewrites that clone's `core.worktree`, as `git submodule update rust`
  did to the primary's fork repository once. A store build refuses such a
  checkout by name before bootstrap runs in it (`sysroot::Fork::checkout`).
- An old-layout sweep takes a name up to its first dot for a key, so Finder's
  `.DS_Store` in a store directory (`rust/build/sysroots/` and
  `rust/build/llvm/` on this host) is the key `""`, whose lock is
  `.git/toyos-build-locks/sysroots/` itself: the sweep panics "build lock: open
  …/toyos-build-locks/sysroots/: Is a directory", as main's `--worktree remove`
  did, and so does every old-layout build that places a key, after placing it.

And the old layout leaves on disk what nothing on the store reads:
`rust/build/<host>/stage2` and the rest of `rust/build/<host>` in the primary
(54G on this host, `du -sh`), `rust/build/x86_64-unknown-toyos`,
`rust/build/toyos-compiler`, `rust/build/toyos-sysroot-claimant`,
`.git/toyos-build-locks`, each worktree's `.build-locks/` (12 on this host) and
the `.build-locks/` line in `.gitignore`.

Exit: no registered worktree on the host builds with a tree older than the
store's landing, and no fork checkout's submodule is a git worktree of another
clone (`git worktree list` in the primary's backtrace clone names that clone
alone); then what is listed above is removed and the `.gitignore` line goes,
in one pull request that closes this.
