---
status: open
kind: tooling
opened: 2026-09-29
---

# The libloading fork labels 0.9.0's code as 0.8.9

Both consumed branches of `ToyOSOrg/rust_libloading` change their base's
`version = "0.9.0"` to `"0.8.9"`: `toyos`, pinned at `fa0abe77` by
`rust/Cargo.lock` (`git diff 35b6a30 fa0abe77 -- Cargo.toml`), and
`toyos-sdk-0.12`, pinned at `bad8d494` by `userland/Cargo.lock` and
`tests/toyos-rust-tests/Cargo.lock`. The base, upstream `35b6a30`, is 0.9.0
plus two upstream commits. The label is what lets `rust/Cargo.toml`'s
`[patch.crates-io]` answer `rust/compiler/rustc_metadata/Cargo.toml`'s
`libloading = "0.8.0"`; the toolchain's crates that ask for 0.9 take the
registry's 0.9.0, which `rust/Cargo.lock` holds beside it. Each of those
lockfiles records the fork as 0.8.9 and builds 0.9.0's API.

**Owner**: the libloading fork.

**Exit**: no consumed branch's `version` differs from the upstream release its
code is, and every lockfile naming one is re-pinned.
