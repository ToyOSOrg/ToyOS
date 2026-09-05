---
status: open
kind: tooling
opened: 2026-09-05
---

# Two toolchain releases report the same `rustc -vV`, so a consumer's cargo keeps the old release's rlibs

The generated `bootstrap.toml` (`src/toolchain.rs`) sets neither `channel`
nor `description` under `[rust]`, so bootstrap's channel is `dev` and its
default for that channel is `omit-git-hash = true`
(`rust/src/bootstrap/src/core/config/config.rs`, the `omit_git_hash` line).
Every release then reports `rustc 1.99.0-dev` with byte-identical `rustc -vV`
output — and the release tag hashes more than `rust` anyway: `toyos-abi/src`,
`toyos/src`, `userland/libc/src`, `toyos-ld` and the packaging.

Cargo fingerprints a compile on `rustc -vV`. A consumer that switches releases
in a reused target directory sees no compiler change, keeps the old rlibs, and
the new rustc refuses their metadata: `error[E0463]: can't find crate` for
`cursor_icon`, `smol_str`, `raw_window_handle`, `keyboard_types` in winit-core.
Recorded at the first external use of the `sdk-0.2.0` alias: Japabu/gbae run
33960322229 restored a target directory built by `5009ee0d7ea1cfad` and built
with `3845b80c8ba72421`. Inside this tree `src/build.rs`'s
`external_fingerprint` is the belt that hides it; a consumer has no belt.

Reproduce with any consumer: build with one release, swap the linked toolchain
to another, build again in the same target directory.

The fix is the release's own name in the compiler's version string —
`description = "<tag>"` in the generated `[rust]` section, which bootstrap
appends to `rustc -vV` — so cargo rebuilds on its own when the release
changes. Until then a consumer keys its cache on the release tag, which is what
gbae does now.
