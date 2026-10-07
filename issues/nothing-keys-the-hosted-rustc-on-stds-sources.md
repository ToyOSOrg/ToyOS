---
status: open
kind: tooling
opened: 2026-10-07
---

# Nothing keys the hosted rustc on std's sources

Nothing keys the hosted rustc on std's sources. `src/toolchain.rs`'s
`hosted_rustc_owed` reads a stamp and whether `bin/rustc` is there, and no std
source: an edit to `sdk/std`, or to the fork's `library/`, leaves a built
hosted rustc standing, and `src/build.rs`'s `collect_hosted_rustc` ships its
`lib/*.so`, the `libstd-*.so` that rustc itself runs on among them, beside the
rlibs of the build's own sysroot. The compiler's key, which is what forgets a
hosted rustc, reads the fork's `compiler/`, `src/tools`, `src/stage0` and
`Cargo.lock` and nothing of `library/`.

It is also unmeasured since std's ToyOS backend left the fork for `sdk/std`:
no hosted rustc has been built whose std reaches the backend through the
fork's `#[path]` arms. Only the primary checkout builds one, in its own
`rust/` (`issues/a-worktree-cannot-build-a-hosted-rustc-of-its-own.md`), so
the branch that moved the backend could not; no tracked config sets
`hosted-rustc = true`, and the primary held no hosted `stage2` when the move
landed. By reading, bootstrap compiles `library/std` in place and the arms
resolve to the primary's `sdk/std`.

Owner: the build system (`src/toolchain.rs`).

**Exit:** on the primary at a `main` that carries the move, a build whose
config asks for the hosted rustc exits 0, and the `libstd-*.so` under
`rust/build/x86_64-unknown-toyos/stage2/lib` carries `sdk/std/sys/` paths and
no `sys/pal/toyos`.
