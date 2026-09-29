---
status: open
kind: tooling
opened: 2026-08-19
---

# The build system does not compile on Windows

Seventy-four errors in the library, in ten files. Measured 2026-09-29 with the
judge below, `--lib --message-format=short`:

| file | errors | the Unix-only call |
|---|---:|---|
| `src/tether.rs` | 36 | a pseudo-terminal per child, signal masks, `setsid`, `pre_exec`, `process_group` |
| `src/dirlock.rs` | 11 | `flock` |
| `src/store.rs` | 9 | mode bits, inode numbers, `kill` |
| `src/signing.rs` | 6 | mode bits |
| `src/icmp.rs` | 5 | the unprivileged ICMP datagram socket |
| `src/sysroot.rs` | 2 | `symlink` |
| `src/firmware.rs` | 2 | mode bits |
| `src/toolchain.rs` | 1 | `symlink` |
| `src/clang.rs` | 1 | `symlink` |
| `src/metaltalk.rs` | 1 | `EHOSTDOWN` |

`libc` itself builds for `x86_64-pc-windows-msvc`. Every other crate in the
graph, first-party and third-party, checked clean.

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
`rustup target add`. It resolves crates.io through the cargo cache, so it is an
on-demand command like `cargo run -- --check-forks`, never `cargo test` and
never the landing gate.

## Compiling is not working, and that is why the cheap half is refused

Each of these wants a Windows call whose semantics differ in kind from the
Unix one it replaces, and none can be run by anybody here:

- `std::os::windows::fs::symlink_dir` needs the privilege or developer mode
  Windows does not grant by default, so `toolchain::swap_link` and
  `sysroot::fork_checkout` would compile and fail at run time — the quieter
  kind of broken.
- `flock` is advisory and whole-file; `LockFileEx` is mandatory and byte-range.
  `dirlock` is what a build waits on behind the maker of a toolchain key, what
  tells a dead maker from a live one, and what keeps a key in use from being
  collected.

So a green Windows compile would say nothing about a working Windows build,
and it would say it in the one subsystem whose failure mode is a collection
removing a toolchain a build is using.

## The self-hosting question underneath

The north star is that nothing rests on a host binary and that everything can
eventually run inside ToyOS. `symlink` is the question in miniature: either
ToyOS grows symbolic links, or the four sites need a shape that does not need
one — a copy, a directory junction, or a sysroot layout that does not require
aliasing a directory at all. Deciding that is worth more than a `#[cfg]` pair,
and it decides four of the seventy-four errors.

## Why this is filed now

The shared-target-directory work
(`issues/build/every-worktree-builds-its-own-copy-of-the-same-crates.md`)
was designed to be portable by construction — a path join, no platform branch
anywhere — on the stated requirement that this project compiles on every major
OS. That requirement is not met today, so the new work would be a portable
component inside a build system with unconditional Unix dependencies at its
centre. Worth knowing before the portability of anything else is claimed.
