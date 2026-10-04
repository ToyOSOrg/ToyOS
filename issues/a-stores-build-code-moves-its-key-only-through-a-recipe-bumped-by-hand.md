---
status: open
kind: tooling
opened: 2026-10-01
---

# A store's build code moves its key only through a RECIPE bumped by hand

A keyed store's key (`src/keystore.rs`) reads its sources, the configuration
its build is given, the tools that run that build and the keys of the stores it
reads. The code that builds it — `src/llvm.rs`, `src/compiler.rs`,
`src/toolchain.rs`, `src/sysroot.rs`, `src/libc.rs`, `src/libcxx.rs` and
`src/clang.rs` — reaches the key only through the store's `RECIPE`, which moves
only when the change's author edits it.

A change that does not keeps the old key. `toolchain.yml` then restores main's
entry under it on every pull request and on main, and no run can save over that
entry, so CI builds with a store the tree's code no longer makes. At
`src/libc.rs`, `.env_remove("RUSTFLAGS")` made
`.env("RUSTFLAGS", "-Coverflow-checks=on")` leaves `sysroot::key` where it was,
and no test reds.

Owner: the keyed stores (`src/keystore.rs`).

**Exit**: a change to the code that builds a keyed store moves that store's key
with no hand edit, and a test reds on the `src/libc.rs` patch above.
