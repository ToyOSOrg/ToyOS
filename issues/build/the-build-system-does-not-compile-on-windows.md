---
status: open
kind: defect
opened: 2026-08-19
---

# The build system does not compile on Windows, and it is three subsystems rather than one call

Seven errors, in `buildlock`, `toolchain` and `worktree`. Measured 2026-09-01:

```
error[E0433]: cannot find `unix` in `os`   src/buildlock.rs:59:14   (AsRawFd)
error[E0433]: cannot find `unix` in `os`   src/toolchain.rs:927:14  (symlink)
error[E0433]: cannot find `unix` in `os`   src/toolchain.rs:1813:14 (symlink)
error[E0425]: cannot find type `statvfs` in crate `libc`      src/worktree.rs:305:24
error[E0425]: cannot find function `statvfs` in crate `libc`  src/worktree.rs:309:29
error[E0599]: no method named `as_raw_fd` found for reference `&std::fs::File`
                                                             src/buildlock.rs:666:32
error[E0599]: no method named `as_raw_fd` found for reference `&std::fs::File`
                                                             src/buildlock.rs:680:32
```

`libc` itself builds for `x86_64-pc-windows-msvc`; it is `statvfs` that is not
there. Every other crate in the graph, first-party and third-party, checked
clean. The `#[cfg(unix)]` at `src/ci.rs:489` is still the only conditional
compilation in the build system.

## The judge, and it needs no Windows host and no download

```
__CARGO_TESTS_ONLY_SRC_ROOT=<scratch> CARGO_TARGET_DIR=<scratch-target> \
  cargo +toyos check -Z build-std=std,panic_abort \
  --target x86_64-pc-windows-msvc --offline -p toyos-build --all-targets
```

`<scratch>` is `src/CLAUDE.md`'s std-src-root recipe with one addition: a
workspace `Cargo.toml` whose members are `library/std`, `library/sysroot`,
`library/proc_macro`, `library/panic_abort` and `library/test`, and whose
`[patch.crates-io]` is `library/Cargo.toml`'s four entries with `library/`
prepended to each path. It works because the fork vendors `library/windows-sys`
and `library/windows_link`, so a Windows `std` builds from the tree — a plain
`cargo check --target x86_64-pc-windows-msvc` instead says *"the
`x86_64-pc-windows-msvc` target may not be installed"* and asks for
`rustup target add`. It resolves crates.io through the cargo cache, so it is an
on-demand command like `cargo run -- --check-forks`, never `cargo test` and
never the landing gate.

## Compiling is not working, and that is why the cheap half is refused

Each of the three wants a Windows call whose semantics differ in kind from the
Unix one it replaces, and none of the three can be run by anybody here:

- `std::os::windows::fs::symlink_dir` needs the privilege or developer mode
  Windows does not grant by default, so `link_host_target` and
  `provision_toolchain_cargo` would compile and fail at run time — the quieter
  kind of broken.
- `flock` is advisory and whole-file; `LockFileEx` is mandatory and byte-range.
  `buildlock` is what serialises the shared sysroot across every worktree.
- `statvfs` against `GetDiskFreeSpaceExW`.

So a green Windows compile would say nothing about a working Windows build,
and it would say it in the one subsystem whose failure mode is two checkouts
silently sharing a sysroot.

## The self-hosting question underneath

The north star is that nothing rests on a host binary and that everything can
eventually run inside ToyOS. `symlink` is the question in miniature: either
ToyOS grows symbolic links, or the two `toolchain.rs` sites need a shape that
does not need one — a copy, a directory junction, or a sysroot layout that does
not require aliasing a directory at all. Deciding that is worth more than a
`#[cfg]` pair, and it decides two of the seven errors.

## Why this is filed now

The shared-target-directory work
(`issues/build/every-worktree-builds-its-own-copy-of-the-same-crates.md`)
was designed to be portable by construction — a path join, no platform branch
anywhere — on the stated requirement that this project compiles on every major
OS. That requirement is not met today, so the new work would be a portable
component inside a build system with unconditional Unix dependencies at its
centre. Worth knowing before the portability of anything else is claimed.
