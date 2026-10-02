---
status: open
kind: tooling
opened: 2026-10-01
---

# A sysroot builds both architectures' userland for a build of one

A sysroot is a whole toolchain for every guest target (`toolchain::GUEST_TARGETS`),
so a new key builds the std set of `x86_64-unknown-toyos` and of
`aarch64-unknown-toyos`, libc for both, and the C++ runtime for both, even when
the build that asked for it is `cargo run -- --build-only`, which links only
x86_64. Measured on the `wt/toyos-rebuild` branch, one item added to
`toyos-abi/src/clock.rs`: the aarch64 std set took 2m18s of a 4m12s std
build at load 41.

**Exit**: a build makes the libraries of only the architectures it builds for,
and the sysroot's key and identity still name exactly what is in it.
