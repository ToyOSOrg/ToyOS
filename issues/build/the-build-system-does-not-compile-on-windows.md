---
status: open
kind: tooling
opened: 2026-08-19
---

# The build system does not compile on Windows

`libc` itself builds for `x86_64-pc-windows-msvc`. Every other crate in the
graph, first-party and third-party, checked clean.

`src/tether.rs`: `std::os::unix` and a pseudo-terminal per child, behind a Linux and macOS `cfg` pair with no Windows arm.

## The judge, and it needs no Windows host and no download

```
__CARGO_TESTS_ONLY_SRC_ROOT=<scratch> CARGO_TARGET_DIR=<scratch-target> \
  cargo +toyos check -Z build-std=std,panic_abort \
  --target x86_64-pc-windows-msvc --offline -p toyos-build --all-targets
```

`<scratch>` is `src/CLAUDE.md`'s std-src-root recipe with two additions: a
workspace `Cargo.toml` whose members are `library/std`, `library/sysroot`,
`library/proc_macro`, `library/panic_abort` and `library/test`, and whose
`[patch.crates-io]` is `library/Cargo.toml`'s four entries with `library/`
prepended to each path; and `library/Cargo.lock` copied beside it, without
which `--offline` resolution refuses the yanked `moto-rt` 0.16.4. It works
because the fork vendors `library/windows-sys`
and `library/windows_link`, so a Windows `std` builds from the tree — a plain
`cargo check --target x86_64-pc-windows-msvc` instead says *"the
`x86_64-pc-windows-msvc` target may not be installed"* and asks for
`rustup target add`. It resolves crates.io through the cargo cache.

## Compiling is not working, and that is why the cheap half is refused

Each wants a Windows call whose semantics differ in kind from the
Unix one it replaces, and none can be run by anybody here:

- `std::os::windows::fs::symlink_dir` needs the privilege or developer mode
  Windows does not grant by default.
- `flock` is advisory and whole-file; `LockFileEx` is mandatory and byte-range.

So a green Windows compile would say nothing about a working Windows build.

## The self-hosting question underneath

The north star is that nothing rests on a host binary and that everything can
eventually run inside ToyOS. `symlink` is the question in miniature: either
ToyOS grows symbolic links, or the sites need a shape that
does not need one — a copy, a directory junction, or a sysroot layout that does
not require aliasing a directory at all. Deciding that is worth more than a
`#[cfg]` pair.
