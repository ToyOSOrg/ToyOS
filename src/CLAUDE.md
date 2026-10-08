# Build system

Loads when you read a file under `src/` — the root cargo project, package name `toyos-build`. Root `CLAUDE.md` has `cargo run` and `cargo test`; the `[profile.toyos]` rule is this crate's to enforce (`src/build.rs`).

## Entry points

- **`.github/qemu-version` is the QEMU every guest is measured with, declared once**; every guest job's first step (`src/ci.rs`'s `instrument`) reds on a disagreement.
- `system.toml` defines which programs to build and what the supervisor starts at boot.

## The host's locks

`src/buildlock.rs`'s header states them and the order every acquirer takes them in, until the build track that holds the machinery cut deletes them.

## Worktrees

- Everything under a worktree — targets, images, `.build-locks/`, its fork checkout — is its own; the object stores are the primary checkout's, and ownership is derived from `git rev-parse --git-common-dir`, never recorded.
- **A linked worktree's `main` ref is only as current as the primary's last `git pull`: anything asking "does this branch differ from main" diffs against `origin/main`.**
- **Type-checking a std edit without building a sysroot**: copy `rust/library` to `<tree>/src/library`, put symlinks to `toyos-abi`, `toyos` and `sdk` in `<tree>`, where std's `../../../` paths look for them, and point `__CARGO_TESTS_ONLY_SRC_ROOT` at the copy itself, whose own workspace and `Cargo.lock` cargo then resolves with — a manifest written above it has no lockfile and does not resolve offline; through the `sdk` link the backend's two `#[path]`s back into the fork read `os_str.rs` and `common.rs` from the worktree's own `rust/library`, never the copy, so that checkout must exist and an edit to either is made there; then, in a scratch crate outside the worktree, `RUSTUP_TOOLCHAIN=<sysroot> CARGO_TARGET_DIR=<scratch> cargo build -Z build-std=std,panic_abort --target x86_64-unknown-toyos --offline`, where `<sysroot>` is the store's `sysroots/<key>` and `<key>` is what the worktree's `target/.deps-stamp` gives `x86_64-unknown-toyos` once it has built. Delete `<scratch>/**/.fingerprint/std-*` between runs — cargo does not re-fingerprint std under `-Zbuild-std`.

## Caveats that bite every agent

- **A workflow job that runs in a container adds `safe.directory` itself** — `actions/checkout` sets it into a temporary global config it discards when its step ends, so the first git command a container step runs after checkout dies on a dubiously-owned repository.
