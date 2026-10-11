# Build system

Loads when you read a file under `src/` — the root cargo project, package name `toyos-build`. Root `CLAUDE.md` has `cargo run` and `cargo test`; the `[profile.toyos]` rule is this crate's to enforce (`src/build.rs`).

## Entry points

- **`.github/qemu-version` is the QEMU every guest is measured with, declared once**; every guest job's first step (`src/ci.rs`'s `instrument`) reds on a disagreement.
- `images/system.toml` defines which programs to build and what the supervisor starts at boot.

## The host's locks

`src/buildlock.rs`'s header states them and the order every acquirer takes them in, until the build track that holds the machinery cut deletes them.

## Worktrees

- Everything under a worktree — targets, images, `.build-locks/`, its fork checkout — is its own; the object stores are the primary checkout's, and ownership is derived from `git rev-parse --git-common-dir`, never recorded.
- **A linked worktree's `main` ref is only as current as the primary's last `git pull`: anything asking "does this branch differ from main" diffs against `origin/main`.**
- **Type-checking a std edit without building a sysroot**: in a scratch library crate outside the worktree, `__CARGO_TESTS_ONLY_SRC_ROOT=<worktree>/rust/library RUSTUP_TOOLCHAIN=<sysroot> CARGO_TARGET_DIR=<scratch> cargo build -Z build-std=std,panic_abort --target x86_64-unknown-toyos --offline`, given std's `[patch]` as the build gives it (`sysroot::std_patch`): `--config 'patch.crates-io.toyos-abi.path="<worktree>/sdk/abi"' --config 'patch.crates-io.toyos.path="<worktree>/sdk/toyos"'`. Cargo resolves the fork's own workspace and `Cargo.lock` with it and writes neither, and std's `#[path]`s read the worktree's `sdk/std`; the warnings that the patch went unused are about the scratch crate's graph. `<sysroot>` is the store's `sysroots/<key>`, `<key>` what the worktree's `target/.deps-stamp` gives `x86_64-unknown-toyos` once it has built. Delete `<scratch>/**/.fingerprint/std-*` between runs — cargo does not re-fingerprint std under `-Zbuild-std`.

## Caveats that bite every agent

- **A workflow job that runs in a container adds `safe.directory` itself** — `actions/checkout` sets it into a temporary global config it discards when its step ends, so the first git command a container step runs after checkout dies on a dubiously-owned repository.
