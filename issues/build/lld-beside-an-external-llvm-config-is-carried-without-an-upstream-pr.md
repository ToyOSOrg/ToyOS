---
status: open
kind: tooling
opened: 2026-09-29
---

# The fork's bootstrap takes LLD from beside an external `llvm-config`, and no upstream pull request numbers it

`ToyOSOrg/rust` commit `d9f8a5ca8db` ("bootstrap: take LLD from beside an
external llvm-config that is Rust's LLVM") accepts `rust.lld = true` with an
external `llvm-config` for the host when that `llvm-config` has
`llvm-has-rust-patches = true` and an `lld` sits in its `bin/`, and takes that
`lld` as `rust-lld`; before it, bootstrap refused the combination. Every
compiler build ToyOS runs relies on it (`src/llvm.rs`).

It is a configuration change, which upstream records in
`src/bootstrap/src/utils/change_tracker.rs` under the number of the pull
request that makes it. No such pull request exists, because none is sent for
now, so the fork carries no entry (`src/forkcheck.rs`).

Exit: an upstream pull request carries the change with its `change_tracker`
entry, and the fork takes it from there.
