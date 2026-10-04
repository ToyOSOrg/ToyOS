---
status: open
kind: tooling
opened: 2026-10-03
---

# Only a metal or debug run builds every binary of the guest test crate

`build_toyos_bins` (`src/build.rs`) is the one build of every binary under
`tests/toyos-rust-tests/src/bin`. It is reached only through
`build_shared_bins` (`tests/toyos.rs`), which runs only under `--metal` and
`--debug`, and no workflow passes either flag. The guest suite builds two of the crate's binaries, each
alone with `build_toyos_bin`'s `--bin`: `abuse_readonly_copyout` and
`panic_halts_first`. `cargo run -- --ci host` builds none of them.

So a test binary that stops compiling lands green, and the next metal session
panics in `cargo_build` before its first boot. At `154054bf5` the crate's
manifest had no `toyos-window`, which `src/bin/window_refusal.rs` imports as
`window`: building every binary as `build_toyos_bins` builds them stopped there
with 13 errors, EXIT=101, and at that head `cargo run -- --ci host` and the
whole guest suite each exited 0.

Owner: the orchestrator.

**Exit**: a check the merge queue runs builds every binary of
`tests/toyos-rust-tests` for x86_64-unknown-toyos, and is red with
`toyos-window` dropped from the crate's manifest.
