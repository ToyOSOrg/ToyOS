---
status: assigned
kind: tooling
opened: 2026-10-08
---

# The primary still holds the toolchain nothing reads any more

Every LLVM, compiler and sysroot is a product of the host's store
(`src/keystore.rs`), and nothing was migrated into it. What the layout before
it left on a development host is read by nothing once the last worktree
still on that layout has merged `main` or gone, and no code removes it,
because no code knows whether such a worktree is still building:

- **The primary's `rust/build`**, 20 G by `du -sh` on the day this was
  filed: the in-place compiler and its build tree (`<host triple>/`, `host`,
  `bootstrap/`, `cache/`, `tmp/`, `lock`), the stores that were in it
  (`llvm/`, `compilers/`, `freestanding/`, `sysroots/`), and
  `toyos-compiler`, the record of the in-place compiler, which is a file.
  `toyos-std/` there is still the primary's std build directory.
- **`toyos-compiler` being a file refuses the primary's first compiler
  build**: `compiler::place` creates `rust/build/toyos-compiler/`, before it
  empties anything, and the OS answers `File exists` on that path, naming no
  cause. It bites the
  first time the primary is the first checkout to name a compiler: a fork
  bump built there, or a host tool update that moves the LLVM's key.
- **`toyos-build-locks/` in the primary's git directory**: the locks of the
  stores above and the global lock.
- **The rustup toolchain `toyos`**, linked to the primary's in-place
  `stage2`. Nothing rebuilds that compiler or its std, so `cargo +toyos` goes
  on running the last ones built there without a word.

**Owner**: the orchestrator, who knows which worktrees are left.

A worktree that has not merged the store still builds with these: it reads
the record to find its compiler, keeps its products in the stores under the
primary's `rust/build` and takes its locks in `toyos-build-locks/`. So
nothing here is safe to
remove while `git worktree list` in the primary names a worktree whose
`src/keystore.rs` has no `pub fn host`, and none while a build runs in the
primary. Removing `toyos-compiler` alone before that, to let the primary
build a compiler, leaves each such worktree refused for want of the record
until it merges. Then, in the primary:

    chmod -R u+w rust/build && rm -rf rust/build
    rm -rf "$(git rev-parse --git-common-dir)/toyos-build-locks"
    rustup toolchain uninstall toyos

The `chmod` is for the old LLVM store, which is read-only. The first build in
the primary afterwards makes `rust/build/toyos-std` again and finds
everything else in the store.

**Exit**: in the primary, `test ! -e rust/build/toyos-compiler -o -d
rust/build/toyos-compiler`, `test ! -e rust/build/sysroots` and `test ! -e
"$(git rev-parse --git-common-dir)/toyos-build-locks"` each exit 0, and
`rustup toolchain list` prints no `toyos`.
