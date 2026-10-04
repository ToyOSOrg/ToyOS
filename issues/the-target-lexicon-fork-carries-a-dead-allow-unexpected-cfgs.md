---
status: open
kind: tooling
opened: 2026-09-29
---

# The target-lexicon fork carries an `allow(unexpected_cfgs)` nothing needs

`ToyOSOrg/target-lexicon` branch `toyos` is pinned at `45832ce6` by
`rust/compiler/rustc_codegen_cranelift/Cargo.lock` and
`tests/toyos-rust-tests/tls-cranelift/Cargo.lock`. Over v0.13.5 it adds
`#![allow(unexpected_cfgs)]` to `build.rs` and `src/lib.rs`, which
`add-toyos-os`, the branch of upstream pull request
bytecodealliance/target-lexicon#134, does not carry. v0.13.5's own
`[lints.rust]` already declares the `feature = "rust_1_40"` that `build.rs`
emits.

**Evidence**: `git archive 45832ce6` with both lines deleted,
`RUSTFLAGS="-D warnings" cargo build --offline` exits 0 under cargo 1.98.1
stable and under `cargo +toyos` (1.96.0-nightly). With the `[lints.rust]`
line deleted as well, the negative control, both exit 101 on
"unexpected `cfg` condition value: `rust_1_40`".

**Owner**: the target-lexicon fork.

**Exit**: both lines are off the `toyos` branch, and both lockfiles are
re-pinned onto it — the first by a commit in `rust/` and its gitlink.
