---
status: open
kind: defect
opened: 2026-09-26
---

# The toolkit forks resolve an x86-only toyos-window, so calc, snake and doom do not build for AArch64

`softbuffer`'s and `winit`'s toyos forks (the `…-sdk-0.2` branches
`userland/Cargo.toml` patches in) name `toyos-window = "0.2"`, and cargo
resolves that to the published `toyos-window 0.2.0`, not to the path crate:
the `[patch]` redirect only applies to a compatible version. That release's
`framebuffer.rs` calls `core::arch::x86_64::_mm_sfence` unconditionally, so it
does not compile for `aarch64-unknown-toyos`, and neither does anything that
pulls either fork in: `calc`, `snake` and `doom`. `cpal`'s fork, `mio`, `tokio`,
`getrandom` and `libloading` name `toyos-abi`/`toyos` 0.1/0.2, which already
select their syscall entry per architecture and build.

`src/build.rs`'s `NOT_YET_BUILT` leaves the three off an AArch64 ROOT and says
so at build time. Everything else in the userland workspace builds and links
for AArch64 (PR #524).

**Exit condition**: the forks name a `toyos-window` version the path crate
satisfies (PR #528 moves them to `…-sdk-0.15` branches for its own reasons, and
the path crate is at 0.16.0 on #524), the three build for
`aarch64-unknown-toyos`, and their rows in `NOT_YET_BUILT` are deleted; doom's
row goes when its C is also shown to compile for AArch64 through `toyos-cc`.
