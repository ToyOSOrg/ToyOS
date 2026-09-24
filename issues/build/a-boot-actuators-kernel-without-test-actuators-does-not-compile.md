---
status: open
kind: tooling
opened: 2026-09-25
---

# A `boot-actuators` kernel without `test-actuators` does not compile

`cargo run -- --build-only --kernel-param quiesce-last-park` builds the kernel
with `boot-actuators` and not `test-actuators`. The build dies on two dead-code
errors under `-D warnings`:

    error: constant `HELD` is never used
       --> src/drivers/panic_console/mod.rs:865:15
    error: function `stalled` is never used
       --> src/drivers/panic_console/mod.rs:887:12

`panic_console::stall` is compiled under `boot-actuators`. Its only reader is
`arch/syscall/debug.rs`'s `await_stalled_painter`, and that is called only from
the `DA::FATAL_HALT` arm of the debug-action dispatch, which is behind
`test-actuators`. The QEMU harness builds both features, so no suite run sees
this. The `--kernel-param` path is the one that stages a metal image with an
actuator armed, so an armed T14 image cannot be built from this tree.

Seen at `wt/toyos-quiesce` merged with `origin/main` at `7c6cad31`. That branch
does not touch `kernel/src/drivers/panic_console`, `debug.rs` or `dispatch.rs`.

**Exit condition**: `cargo run -- --build-only --kernel-param <any actuator>`
builds, and a gate builds that feature set so it stays built.
