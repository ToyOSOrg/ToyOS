---
status: open
kind: tooling
opened: 2026-10-09
---

# No required check builds the shipped image for AArch64

`cargo run -- --build-only --arch aarch64` builds every program of
`system.toml` for `aarch64-unknown-toyos` and puts it on the AArch64 ROOT, and
nothing a pull request or the merge queue runs issues it. `nightly.yml`'s two
`--build-only` steps name no architecture and build x86-64. `src/clippy.rs`
lints the kernel and the loader for AArch64 and no userland program. The
`virt_` guest tests build `tests/virt*case/system.toml` or
`tests/testcases/system.toml`, which name servers and test programs and no windowed
one.

The build has no per-architecture list of programs left out: a program that
stops building for AArch64 fails that build by name. So the failure is loud, and
found only by whoever next runs that build by hand.

Owner: the build system (`src/ci.rs` and `.github/workflows/`).

**Exit condition.** A required check runs `cargo run -- --build-only --arch
aarch64`, or its equivalent, on the shipped `system.toml`, and is red when one
of its programs does not build for AArch64.
