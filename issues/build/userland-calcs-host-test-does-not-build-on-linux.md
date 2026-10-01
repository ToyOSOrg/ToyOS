---
status: open
kind: tooling
opened: 2026-10-01
---

# `userland/calc`'s host test does not build on Linux

`cargo run -- --ci host` runs `cargo test --manifest-path userland/calc/Cargo.toml
--target <host>`. That builds the window binary as well as the engine: its
`main.rs` carries tests of its own, and `tests/parser_identities.rs` is an
integration test. The binary depends on `winit` and `softbuffer` with default
features off, which is right for ToyOS. On macOS both crates have a backend
with no feature, and on Linux neither does. Run 36839912041 of PR #667, on
`ubuntu-24.04`, failed in `softbuffer`: 25 errors over a backend dispatch enum
that has no variants. A local `cargo check --target x86_64-unknown-linux-gnu
--keep-going` fails in both crates: winit says "The platform you're compiling
for is not supported by winit", and softbuffer fails with the same 25 errors.

There are three ways out, and each needs a ruling:

- Turn on a Linux backend (`x11` or `wayland`) for both crates under a Linux
  target `cfg` in calc's manifest. That adds crates to `userland/Cargo.lock`.
- Let both forks compile with no backend on Linux, failing at run time instead.
  That is a cross-platform change to two forks.
- Move the engine and its tests into a crate with no window dependency. That
  still leaves the layout tests in `main.rs` needing the window crates, and
  `src/userlandhost.rs` gates only a crate whose tests run under a bare
  `cargo test`.

**Exit**: the userland/calc step of `cargo run -- --ci host` is green on
`ubuntu-24.04`.
