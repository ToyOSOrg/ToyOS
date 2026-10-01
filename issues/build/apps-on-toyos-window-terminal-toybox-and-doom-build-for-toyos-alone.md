---
status: open
kind: defect
opened: 2026-10-01
---

# Apps on toyos-window, terminal, toybox and doom build or run on ToyOS alone

A userland app builds and runs on macOS, Linux, Windows and ToyOS from the same
source (owner's ruling). These do not. Each row below is
`cargo check --manifest-path userland/<app>/Cargo.toml --target x86_64-unknown-linux-gnu`,
run on the macOS dev host:

| app | exit | what stops it |
|---|---|---|
| editor, files, paint, filepicker | 0 | they compile, but `toyos-abi/src/syscall.rs` issues ToyOS's `syscall` on every OS, so `toyos-window` reaches no display off ToyOS |
| terminal | 101 | "unresolved import `std::os::toyos`" |
| toybox, doom | 101 | `alsa-sys`, which cpal's Linux backend links: its `build.rs` finds ALSA only through `pkg-config`, so a Linux build needs `libasound2-dev` installed |

doom's `build.rs` also panics on any target the build system does not name a C
compiler for. With `--target x86_64-pc-windows-msvc` it exits 101 with
"CC_x86_64_pc_windows_msvc is unset".

**Exit:** each builds with a plain `cargo build` for Linux, macOS and Windows,
and opens its window there.
