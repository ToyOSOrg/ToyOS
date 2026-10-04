---
status: open
kind: tooling
opened: 2026-09-29
---

# No userland program is linted, though the gated ones build for the host

`src/clippy.rs` lints each crate nested under a userland program that
`src/userlandhost.rs`'s survey gates, against the host, and no userland program.
The `toyos` toolchain ships no clippy, but the programs the survey gates already
build for the host with the stable toolchain, and stable's clippy lints them
there. They are not clean:
`cargo clippy --manifest-path userland/<crate>/Cargo.toml --target
aarch64-apple-darwin --all-targets -- -D warnings` exits 101 with 8 findings
in soundd and 10 in netd. The compositor stops on 1 finding in its dependency
`userland/toyos-window`.

**Exit:** a shape in `src/clippy.rs`, which `--clippy` and `--ci host` both
run, lints every crate the survey gates, on the host target, with warnings
denied, and those findings are fixed. A clippy finding planted in a gated
userland crate reds `--clippy`.
