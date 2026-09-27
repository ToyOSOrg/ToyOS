---
status: open
kind: tooling
opened: 2026-09-27
---

# No gate runs clippy over the compositor

`userland/compositor/src/main.rs` denies `clippy::undocumented_unsafe_blocks`,
and nothing in `src/clippy.rs` or `.github/workflows/` runs clippy over
`userland/`, so the attribute holds only when someone runs it by hand. The
`toyos` toolchain has no clippy, but the compositor checks with the host's:

    cd userland && cargo +stable clippy -p compositor --no-deps \
        --target aarch64-apple-darwin -- -A clippy::all

exits 0 and reds on an unsafe block with no `SAFETY:` comment. Without
`-A clippy::all` it reds on one default lint in `Session::tick_taskbar`.

**Exit**: a shape in `src/clippy.rs` runs that command for the compositor on
every host architecture the gate runs on, and fails on its findings.
