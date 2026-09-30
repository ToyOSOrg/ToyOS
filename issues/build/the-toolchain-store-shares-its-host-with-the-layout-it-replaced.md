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
  there). A build in one whose backtrace gitlink moved to a commit that clone
  lacks has bootstrap run `git submodule update` over it, which rewrites that
  clone's `core.worktree`, as `git submodule update rust` did to the primary's
  fork repository once. The store's own checkouts take no submodule from the
  primary.

And the old layout leaves on disk what nothing on the store reads:
`rust/build/<host>/stage2` and the rest of `rust/build/<host>` in the primary
(54G on this host, `du -sh`), `rust/build/x86_64-unknown-toyos`,
`rust/build/toyos-compiler`, `rust/build/toyos-sysroot-claimant`,
`.git/toyos-build-locks`, each worktree's `.build-locks/` (12 on this host) and
the `.build-locks/` line in `.gitignore`.

Exit: no registered worktree on the host builds with a tree older than the
store's landing; then what is listed above is removed and the `.gitignore`
line goes, in one pull request that closes this.
