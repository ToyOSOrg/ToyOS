---
status: open
kind: tooling
opened: 2026-09-27
---

# A sysroot cloned while stage2 had no cargo stays broken for its key

`sysroot::build` (`src/sysroot.rs`) makes a sysroot by `clone_tree` of the
primary's `stage2`, and only `provision_toolchain_cargo` (`src/toolchain.rs`)
puts `bin/cargo` there. A sysroot cloned in the window between a compiler
rebuild and that provisioning has no `cargo`; `finished()` reads only
`SOURCES`, so nothing rebuilds it, and every build against its key panics in
`assert_toolchain_is_honest`.

## Evidence

On the dev host, primary at `c5518949`:

- `rust/build/aarch64-apple-darwin/stage2/` modified 22:00:14,
  `stage2/bin/rustdoc` 21:55, `stage2/bin/cargo` (the link to
  `nightly-aarch64-apple-darwin/bin/cargo`) made 22:08:04.
- `rust/build/sysroots/5dc157f7fac727be/` and its `SOURCES` 22:03:59, built by
  pid 20399 (`[build-lock] ... held by pid 20399 (building sysroot
  5dc157f7fac727be)`), `SOURCES` naming `fork
  /Users/jan/Dev/jan/toyos-nokthread/rust`. Its `bin/` holds `rustc` and
  `rustdoc` and no `cargo`.
- `cargo test --test toyos-build -- --nightly xhci_flap` in
  `/Users/jan/Dev/jan/toyos-xhciwake` then exits 101 before any guest boots:
  every C corpus entry reports `the toyos toolchain at
  .../sysroots/5dc157f7fac727be/bin is missing cargo` (log finished 22:04:13).
  The same command in the same worktree exited 0 in a run whose log finished
  21:55:26.

Which step left `stage2` without its link between 22:00 and 22:08 is not
measured.

## Exit condition

A sysroot is never marked finished without the `cargo` its toolchain needs, or
one found without it is rebuilt or provisioned where `ensure` finds it. Owner:
`src/sysroot.rs` and `src/toolchain.rs`; held by the orchestrator.
