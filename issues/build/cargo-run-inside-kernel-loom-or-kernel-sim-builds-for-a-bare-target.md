---
status: assigned
kind: tooling
opened: 2026-10-03
---

# Cargo run inside `kernel/loom` or `kernel/sim` builds for a bare target

Cargo reads `.cargo/config.toml` from the working directory up, so a `cargo
test` run inside `kernel/loom` or `kernel/sim` takes `kernel/.cargo/config.toml`'s
`build.target = "x86_64-unknown-none"` and its `rustflags`. Run from the
repository root, `-p kernel-loom` and `-p kernel-sim` build for the host, which
is how `--ci host` runs them.

**Evidence:** inside each, `cargo test --no-run` exits 101 on `can't find crate
for std` and `the x86_64-unknown-none target may not support the standard
library`: in `kernel/loom` on loom's own dependencies (`scoped-tls`, `log`,
`once_cell`), in `kernel/sim` on `getrandom`. A `.cargo/config.toml` of the
package's own with `build.target = "host-tuple"` overrides it (measured on
cargo 1.98.1), but builds into `target/<host triple>/` with the kernel's
`rustflags`: a second build of every crate the root's run builds.

**Exit:** `cargo test` run inside `kernel/loom` and inside `kernel/sim` builds
for the host and is green. Held by the orchestrator, who put both under
`kernel/`.
