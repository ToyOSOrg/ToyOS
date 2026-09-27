---
status: open
kind: tooling
opened: 2026-09-27
---

# A sysroot cloned while the compiler's `bin/` lacks `cargo` is finished, and refused by every build that names it

`src/sysroot.rs`'s `build` clones `compiler.stage2` into the partial sysroot,
places std and libc, writes `SOURCES` and renames it into place: finished, and
never written again. `ensure` runs `toolchain::assert_toolchain_is_honest` on
it only afterwards, on every use. A clone taken while the stage2 `bin/` has no
`cargo` is therefore a sysroot that every build naming its key refuses, and
that nothing rebuilds.

Seen on 2026-09-27. `rust/build/sysroots/5dc157f7fac727be/` has `SOURCES` at
22:03:59, naming the fork `…/toyos-nokthread/rust`. Its `bin/` holds `rustc`
and `rustdoc` and no `cargo`. The primary's
`rust/build/aarch64-apple-darwin/stage2/` was re-made at 22:00, and its
`bin/cargo` link dates from 22:08.

That key comes from `origin/main` c5518949's sources, so every worktree at main
with no ABI change names it. In `toyos-transport`, `cargo test --test
toyos-build -- --nightly blockd_survives_its_death` panicked at
`tests/toyos.rs:2972` before any guest ran: every C corpus case "no longer
links … the toyos toolchain at …/sysroots/5dc157f7fac727be/bin is missing
cargo".

**Exit condition**: the honesty check runs on the partial sysroot before
`SOURCES` is written, so a sysroot without `cargo` is never finished.
