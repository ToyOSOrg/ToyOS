---
status: open
kind: tooling
opened: 2026-09-28
---

# The toolchain install unpacks an asset no digest vouches for

`release::install` (`src/release.rs`) downloads the `toyos-toolchain.tar.zst`
asset of the release its tree's tag names, unpacks it into `rust/build` and
links it as rustup's `toyos`, and checks nothing about its bytes: the tag is
the hash of the tarball's inputs (`trees`), not of the tarball, and the
`toyos-sysroot-witness` an installed toolchain is held to comes inside it.
Every guest job compiles and boots with what it installs.

A job holding this repository's `contents: write` token can replace a
release asset, so any code such a job runs can replace the toolchain every
later job installs. The nightly's `build` job holds that token while it
bootstraps the toolchain.

Owner: the release module (`src/release.rs`).

**Exit condition.** `install` unpacks only an asset whose SHA-256 is one that
no job holding a write token can rewrite — committed to the tree it installs
for — and refuses any other by name.
