---
status: open
kind: tooling
opened: 2026-09-27
---

# A sysroot made between the toolchain's rebuild and its cargo has none, for good

`src/toolchain.rs` rebuilds the primary's toolchain in steps that each take
the global lock and let it go: "build the ToyOS-hosted rustc" recreates
`stage2/bin/`, and "give the toyos toolchain its own cargo" puts `bin/cargo`
back afterwards, under a lock of its own. A sysroot build in another worktree
(`src/sysroot.rs`'s `build`) takes the compiler lock shared between the two,
clones `stage2/` with no `cargo` in it, and writes the sysroot finished. A
sysroot is never written again, so every later build of that key refuses at
`assert_toolchain_is_honest`: "the toyos toolchain at …/sysroots/<key>/bin is
missing cargo", and a re-run refuses the same way.

Measured on 2026-09-27: `sysroots/5dc157f7fac727be/bin` (22:03) and
`sysroots/e8d0e10f5498e275/bin` (22:07) hold `rustc` and `rustdoc` and no
`cargo`; the primary's `stage2/bin/cargo` link is dated 22:08.

## Exit condition

No sysroot can clone `stage2/` while its `bin/` is being remade: the rebuild
and the cargo that goes with it are one step under one hold of the lock, or
the clone refuses a `bin/` without `cargo` before it writes the sysroot.
