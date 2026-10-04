---
status: open
kind: tooling
opened: 2026-09-29
---

# The ctrlc fork labels 3.5.2's code as 3.5.1

`rust/Cargo.lock` pins `ToyOSOrg/rust-ctrlc` branch `toyos` at `fb7e8988`,
version 3.5.1. The branch sits on upstream's `3.5.2` tag (`0aed47c`, "Release
3.5.2"), and `git diff 3.5.2 fb7e8988 -- Cargo.toml` changes
`version = "3.5.2"` to `"3.5.1"` — the version upstream Rust's lock names
(`rust/Cargo.lock` at `b04d3c8c`, the fork's merge-base with rust-lang/rust).
Its one consumer, `rust/compiler/rustc_driver_impl/Cargo.toml`, asks for
`ctrlc = "3.4.4"`, which 3.5.2 meets.

**Owner**: the toolchain fork's dependencies, `rust/Cargo.toml`'s
`[patch.crates-io]`.

**Exit**: the branch says `3.5.2`, and `rust/Cargo.lock` is re-locked onto it.
