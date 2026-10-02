---
status: open
kind: tooling
opened: 2026-10-01
---

# Every sysroot key compiles userland's `core` and `alloc` again

`sysroot::build_std` empties each userland target's `stage0-std/` before it
builds (`prepare_std_build`), so every new sysroot key compiles `core`, `alloc`
and `compiler_builtins` for both userland targets from nothing, although they
read nothing of the worktree: in this branch's build of
`x86_64-unknown-toyos`, cargo's dep-info for them (`dist/libcore.d`,
`liballoc.d`, `libcompiler_builtins.d`) names 411, 714 and 630 paths, none in
the worktree outside its fork, where `libstd.d` names 42. The emptying rests
on the claim, in a comment there, that bootstrap does not see a path
dependency outside the fork move; nobody has measured a build that keeps the
directory. Measured on the `wt/toyos-rebuild` branch, one item added to
`toyos-abi/src/clock.rs`, from the timestamped build log: `core` and `alloc`
were the first 37 s of the 91 s `x86_64-unknown-toyos` std build, and 91 s of
the 158 s `aarch64-unknown-toyos` one, at a load that peaked at 186.

**Exit**: one item added to `toyos-abi/src/clock.rs` after a build, and the
next `cargo run -- --build-only` prints no `Compiling core` and no
`Compiling alloc`.
