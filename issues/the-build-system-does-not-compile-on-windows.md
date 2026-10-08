---
status: open
kind: tooling
opened: 2026-08-19
---

# The build system does not compile on Windows, and it is three subsystems rather than one call

`src/tether.rs`: `std::os::unix` and a pseudo-terminal per child, behind a Linux and macOS `cfg` pair with no Windows arm.

## The judge needs no Windows host, and stops at a dependency's C before it reaches the build system

```
__CARGO_TESTS_ONLY_SRC_ROOT=<tree>/src/library CARGO_TARGET_DIR=<scratch-target> \
  RUSTUP_TOOLCHAIN=<sysroot> cargo check -Z build-std=std,panic_abort \
  --target x86_64-pc-windows-msvc --offline -p toyos-build --all-targets
```

`<tree>` and `<sysroot>` are `src/CLAUDE.md`'s std type-check recipe's, with
no addition: the copied library is its own workspace, with its patches and
its lockfile, and a manifest written above it does not resolve offline. The
fork vendors `library/windows-sys` and `library/windows_link`, so a Windows
`std` builds from the tree — a plain `cargo check --target
x86_64-pc-windows-msvc` instead says *"the `x86_64-pc-windows-msvc` target may
not be installed"* and asks for `rustup target add`. It resolves crates.io
through the cargo cache.

Run that way it builds `core`, `std` and `test` for Windows and exits 101 at
the 37th crate, before any source of `toyos-build` is checked: `failed to run
custom build command for ring v0.17.14`, whose build script compiles C for
the target with the host's `cc` and finds no `assert.h`. The judge reaches
`src/tether.rs` only with the target's C headers, the C runtime's and the
Windows SDK's, which no host here has and which are a download; or once
nothing `toyos-build` depends on compiles C for its target.

## Compiling is not working, and that is why the cheap half is refused

Each wants a Windows call whose semantics differ in kind from the
Unix one it replaces, and none can be run by anybody here:

- `std::os::windows::fs::symlink_dir` needs the privilege or developer mode
  Windows does not grant by default, so `provision_toolchain_cargo` would
  compile and fail at run time — the quieter kind of broken.
- `flock` is advisory and whole-file; `LockFileEx` is mandatory and byte-range.
  `buildlock` is what serialises the shared sysroot across every worktree.

So a green Windows compile would say nothing about a working Windows build,
and it would say it in the one subsystem whose failure mode is two checkouts
silently sharing a sysroot.

## The self-hosting question underneath

The north star is that nothing rests on a host binary and that everything can
eventually run inside ToyOS. `symlink` is the question in miniature: either
ToyOS grows symbolic links, or the two `toolchain.rs` sites need a shape that
does not need one — a copy, a directory junction, or a sysroot layout that does
not require aliasing a directory at all. Deciding that is worth more than a
`#[cfg]` pair.
