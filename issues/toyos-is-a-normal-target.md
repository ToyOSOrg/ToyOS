---
status: open
kind: track
opened: 2026-09-03
---

# ToyOS is a normal target for software

A clean clone of any third-party program that targets ToyOS cannot resolve its
dependencies on Linux or macOS, because cargo resolves every platform's
dependencies and the ToyOS crates the forks name — `toyos-abi`, `toyos`,
`toyos-window`, `toyos-osrelease` — are on no registry. A git dependency on the
monorepo is not the answer: it clones the `rust` submodule. So the SDK crates,
`src/sdkversion.rs` `PUBLISHED`, go on crates.io and stay there. The ABI they carry is unstable by owner ruling: a built program
that breaks, breaks.

Stages, in order:

1. **Done.** `toyos-abi`, `toyos-keymap`, `toyos-font`, `toyos` and
   `toyos-window` carry a description and a repository, and are published by
   `.github/workflows/publish.yml` through crates.io trusted publishing.
2. **The owner's: `toyos-osrelease`'s first publish.** It is in `PUBLISHED`,
   and the sysinfo fork names it by version, but crates.io takes a crate's
   first version only with an API token, and trusted publishing is configured
   per crate after it. Until then main's `publish` job is red on every landing,
   refused at `toyos-osrelease`, which is `PUBLISHED`'s last row so that every
   crate above it still goes up; and the sysinfo fork resolves only inside this
   workspace, through its `[patch.crates-io]`. The owner publishes it once with
   a token and names `publish.yml` its trusted publisher. Exit:
   `https://index.crates.io/to/yo/toyos-osrelease` answers 200, main's next
   `publish` is green, and the row's "last until crates.io holds it" comment
   is deleted.
3. **The forks.** softbuffer names
   `toyos-window` and sits on the v0.4.8 release, and raw-window-handle sits on
   v0.6.2, so nothing the window path goes through is based on a master any
   more.
4. **Done.** The toolchain is a release a consumer can name, install and link
   with. `toolchain-linux-x86_64-sdk-<toyos-abi's version>` is the tag it pins —
   the SDK version names the ABI, and the toolchain that goes with it carries
   the same number — and that release's asset is the `TOOLCHAIN` manifest, which
   names the content-keyed release the tarball is on. What a consumer runs, and
   the release notes of every toolchain release carry it:

       mkdir -p toyos-toolchain
       curl -sSL "$asset" | tar -xz -C toyos-toolchain
       stage2=toyos-toolchain/x86_64-unknown-linux-gnu/stage2
       rustup toolchain link toyos "$stage2"
       ln -s "$(rustup which cargo)" "$stage2/bin/cargo"
       export PATH="$PATH:$PWD/$stage2/bin"
       cargo +toyos build --target x86_64-unknown-toyos

   The glibc floor is 2.39 — `ubuntu-24.04`'s, the
   image the host half is built on — measured over the shipped binaries and
   asserted at publish time, so a build on a newer machine is refused rather
   than published. A program that opens a window also carries a `[patch]` of
   `raw-window-handle` to the fork's release branch, until
   rust-windowing/raw-window-handle#223 is released.
5. **Upstream.** The three backends — winit-toyos, softbuffer's ToyOS backend,
   cpal's ToyOS host — become upstream pull requests rather than forks.
6. **The horizon.** `x86_64-unknown-toyos` as a target in upstream rustc, which
   is what ends the `rust/` fork. Nothing here depends on it and everything here
   is a step toward it.
