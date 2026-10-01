---
status: open
kind: defect
opened: 2026-10-01
---

# Apps that build or run on ToyOS alone

An app builds and runs on Linux under Wayland, macOS and Windows from the same
source as on ToyOS (`userland/CLAUDE.md`). These build on none of the
three, and each manifest's `[package.metadata.toyos.host] fails` names all
three, which `cargo run -- --ci host` holds to the build:

| app | what stops it |
|---|---|
| `doom` | its `build.rs` panics unless the build system names a C compiler in `CC_<target>`; on Linux, cpal's `alsa-sys` finds ALSA only through `pkg-config`, so a build needs `libasound2-dev` |
| `toybox` | `std::os::toyos` in its `stats` and `locale` applets; on Linux, cpal's `alsa-sys`, as doom |
| `terminal` | `std::os::toyos`: it starts its shell with `CommandExt::provide` |
| `shell` | `std::os::toyos`: raw stdin, and `CommandExt::provide` to hand each program the connectors the shell was given |
| `proctest` | `std::os::toyos::io::set_stdin_raw`, and it spawns `/system/bin/echo` and itself by their ToyOS paths |

`editor`, `files` and `paint` build on all three and run on none:
`toyos-window` reaches no display but ToyOS's compositor, because
`toyos-abi/src/syscall.rs` issues ToyOS's `syscall` on every OS.

**Exit:** no manifest names this file, and each app above runs on every host.
