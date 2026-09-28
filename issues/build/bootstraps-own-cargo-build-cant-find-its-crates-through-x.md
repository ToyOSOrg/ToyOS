---
status: open
kind: tooling
opened: 2026-09-28
---

# Bootstrap's own `cargo build`, run through `x`, can't find its crates

`cargo run -- --build-only` and `cargo test --test toyos-build -- --list`
both panic at `src/toolchain.rs:650` ("std did not compile") on a worktree
that just moved its `rust/` pin: `./x build library --stage 0 --config
<build-dir>/bootstrap.toml ...` fails compiling `src/bootstrap` itself with
`error[E0463]: can't find crate for` `serde`, `clap`, `xz2`, `sha2`, `object`,
`ignore`, `clap_complete`, `cc`, `termcolor`, `build_helper` and others —
33 errors, `Compiling bootstrap v0.0.0` printed with no dependency compiled
before it, and `Build completed unsuccessfully in 0:00:00`.

**Reproduced with `./x` alone**, bypassing `src/toolchain.rs` entirely, so
the cause is in `rust/`'s own bootstrap or its build-dir, not in this
repository's wrapper. Every one of the missing crates, at the exact version
`src/bootstrap/Cargo.lock` names, is already present under
`~/.cargo/registry/src/`.

**The same `cargo build` command `x` logs as having failed** — copied
verbatim, including `RUSTC`, `RUSTC_BOOTSTRAP=1`, `RUSTFLAGS=-Zallow-features=`
and `CARGO_TARGET_DIR` set to the exact same
`<build-dir>/bootstrap` — compiles cleanly when run directly, both against
that same target directory and against a fresh one, every time it was tried
by hand. Matching `x`'s environment as closely as `ps eww` could show
(`RUSTUP_TOOLCHAIN`, `CARGO`, a fully cleared environment) did not reproduce
the failure outside `x`; only going through `x` itself does, on every one of
eight tries across roughly forty minutes of otherwise-varying host load —
including once with almost no other build running on the machine — so this
is not the host-load flakiness `buildlock`'s test suite already knows about.

**Exit**: reproduce standalone (`./x build library --stage 0 --config
<build-dir>/bootstrap.toml ...` on a `local-rebuild = true` config after
`rust/`'s pin moves), and trace what `x`'s own `cargo build` invocation for
`src/bootstrap` does differently from the identical command line run by
hand — most likely a stale `.fingerprint` entry under `<build-dir>/bootstrap`
that a hand-run `cargo build` invalidates correctly and `x`'s does not.
