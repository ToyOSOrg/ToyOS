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
3. **The forks.** `forks.toml`'s `owed` per fork. softbuffer names
   `toyos-window` and sits on the v0.4.8 release; raw-window-handle is the last
   fork based on a master rather than a release, and its `owed` says what moving
   it off costs. Every fork whose `pr` says "sendable once … is on crates.io"
   becomes sendable.
4. **The toolchain.** A ToyOS rustc/std is what a third-party program needs
   after the crates resolve, and building one is a two-hour bootstrap of the
   `rust/` fork. `toolchain.yml` already builds one and uploads it as a release
   asset, and `.github/install-toolchain.sh` fetches it — but the tag is a
   content key CI computes for its own cache. What is owed is a release named
   for a human and a one-line install anybody outside this repository can run.
5. **Upstream.** The three backends — winit-toyos, softbuffer's ToyOS backend,
   cpal's ToyOS host — become upstream pull requests rather than forks, which is
   what the `sibling` tier in `forks.toml` means.
6. **The horizon.** `x86_64-unknown-toyos` as a target in upstream rustc, which
   is what ends the `rust/` fork. Nothing here depends on it and everything here
   is a step toward it.
