---
status: open
kind: tooling
opened: 2026-09-28
---

# A worktree's bootstrap cache outlives a compiler rebuilt at the same version

A linked worktree builds its std sysroot through its own fork checkout's
bootstrap, whose cargo target directory is
`rust/build/toyos-std/bootstrap`, compiled by the primary's compiler named in
`bootstrap.toml`'s `rustc`. Every build of that compiler answers `rustc
1.99.0-dev`, `commit-hash: unknown`. Cargo keys freshness on that answer, so
once the primary's compiler is rebuilt,
the cached dependencies are reused, and the new
compiler cannot load them. Bootstrap then fails to compile with `error[E0463]:
can't find crate for serde` (and `clap`, `xz2`), with nothing compiled before
it, and the sysroot build panics at `src/toolchain.rs` with "std did not
compile".

**Evidence:** PR #536's worktree at 1f969973, sysroot key 8ec6e31551091ba8:
`cargo run -- --build-only` failed the same way twice. Its
`libserde-*.rmeta` dated from 2026-09-27; the compiler directory
`compilers/a04a68b92a50e478` from 2026-09-28 09:07. The same sources built
cleanly into a fresh target directory. With the stale directory moved aside,
the next `--build-only` succeeded.

**Exit condition:** a worktree's bootstrap cache is keyed on, or cleared with,
the compiler that builds it, so a rebuilt compiler never meets artifacts from
the one it replaced.
