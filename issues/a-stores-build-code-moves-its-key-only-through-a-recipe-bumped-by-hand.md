---
status: open
kind: tooling
opened: 2026-10-01
---

# A store's build code moves its key only through a RECIPE bumped by hand

A keyed store's key (`build/keystore.rs`) reads its sources, the configuration
its build is given, the tools that run that build and the keys of the stores it
reads. The code that builds it — `build/llvm.rs`, `build/compiler.rs`,
`build/toolchain.rs`, `build/sysroot.rs`, `build/libc.rs`, `build/libcxx.rs` and
`build/clang.rs` — reaches the key only through the store's `RECIPE`, which moves
only when the change's author edits it.

A change that does not keeps the old key. `toolchain.yml` then restores main's
entry under it on every pull request and on main, and no run can save over that
entry, so CI builds with a store the tree's code no longer makes. At
`build/libc.rs`, `.env_remove("RUSTFLAGS")` made
`.env("RUSTFLAGS", "-Coverflow-checks=on")` leaves `sysroot::key` where it was,
and no test reds.

Owner: the keyed stores (`build/keystore.rs`).

**Exit**: a change to the code that builds a keyed store moves that store's key
with no hand edit, and a test reds on the `build/libc.rs` patch above.
