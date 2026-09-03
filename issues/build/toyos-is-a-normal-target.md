---
status: open
kind: track
opened: 2026-09-03
---

# ToyOS is a normal target for software

A clean clone of any third-party program that targets ToyOS cannot resolve its
dependencies on Linux or macOS, because cargo resolves every platform's
dependencies and the ToyOS crates the forks name — `toyos-abi`, `toyos`,
`toyos-window` — are on no registry. A git dependency on the monorepo is not the
answer: it clones the `rust` submodule. So the five SDK crates go on crates.io
and stay there. The ABI they carry is unstable by owner ruling: a built program
that breaks, breaks.

Stages, in order:

1. **Done.** `toyos-abi`, `toyos-keymap`, `toyos-font`, `toyos` and
   `toyos-window` carry a description and a repository, name each other by
   version, and are published by `.github/workflows/publish.yml`;
   `src/sdkversion.rs` refuses a branch that changes one without bumping it.
2. **The owner's.** `CARGO_REGISTRY_TOKEN` as a repository secret, then the
   first publish. Until it is there the publish job fails by name on every
   landing, which is the intended noise.
3. **The forks.** `forks.toml`'s `owed` per fork: winit and softbuffer must
   name `toyos-window` instead of `window`, and until they do the monorepo does
   not build — the two are one change across three repositories. Then softbuffer
   is rebased off master onto v0.4.8 (`forks.toml`'s `rebase`), and every fork
   whose `pr` says "sendable once … is on crates.io" becomes sendable.
4. **Done.** The toolchain is a release a consumer can name, install and link
   with. `toolchain-linux-x86_64-sdk-<toyos-abi's version>` is the tag it pins —
   the SDK version names the ABI, and the toolchain that goes with it carries
   the same number — and that release's asset is the `TOOLCHAIN` manifest, which
   names the content-keyed release the tarball is on. What a consumer runs, and
   the release notes of every toolchain release carry it:

       mkdir -p toyos-toolchain
       curl -sSL "$asset" | tar --zstd -x -C toyos-toolchain
       stage2=toyos-toolchain/x86_64-unknown-linux-gnu/stage2
       rustup toolchain link toyos "$stage2"
       ln -s "$(rustup which cargo)" "$stage2/bin/cargo"
       export PATH="$PWD/$stage2/bin:$PATH"
       cargo +toyos build --target x86_64-unknown-toyos

   `toyos-ld` is in that `bin/` because rustc's ToyOS target names its linker
   and finds it on `PATH`. The glibc floor is 2.39 — `ubuntu-24.04`'s, the
   image the host half is built on — measured over the shipped binaries and
   asserted at publish time, so a build on a newer machine is refused rather
   than published. A program that opens a window also carries a `[patch]` of
   `raw-window-handle` to the fork's release branch, until
   rust-windowing/raw-window-handle#223 is released.
5. **Upstream.** The three backends — winit-toyos, softbuffer's ToyOS backend,
   cpal's ToyOS host — become upstream pull requests rather than forks, which is
   what the `sibling` tier in `forks.toml` means.
6. **The horizon.** `x86_64-unknown-toyos` as a target in upstream rustc, which
   is what ends the `rust/` fork. Nothing here depends on it and everything here
   is a step toward it.
