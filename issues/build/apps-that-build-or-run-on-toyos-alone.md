---
status: open
kind: defect
opened: 2026-10-01
---

# Apps that build or run on ToyOS alone

An app builds and runs on Linux under Wayland, macOS and Windows from the same
source as on ToyOS (`userland/CLAUDE.md`). These build on none of the three:
each manifest's `[package.metadata.toyos.host] fails` names all three, so
`cargo run -- --ci host` checks none of them there.

| app | what stops it |
|---|---|
| `doom` | its `build.rs` compiles doomgeneric's C with the compiler the build system names in `CC_<target>`, and panics without one; a check of another host's triple has no C library for it. On Linux, cpal's `alsa-sys` links ALSA's C library, found through `pkg-config`, and neither is a declared host tool (`issues/build/the-build-runs-host-tools-outside-rust-and-qemu.md`) |
| `toybox` | `std::os::toyos` in its `stats` and `locale` applets; on Linux, cpal's `alsa-sys`, as doom |
| `terminal` | `std::os::toyos`: it starts its shell with `CommandExt::provide` |
| `shell` | `std::os::toyos`: raw stdin, and `CommandExt::provide` to hand each program the connectors the shell was given |
| `proctest` | `std::os::toyos::io::set_stdin_raw`, and it spawns `/system/bin/echo` and itself by their ToyOS paths |

`editor`, `files`, `paint` and `filepicker` build on all three and run on none:
`toyos-window` reaches no display but ToyOS's compositor, because
`toyos-abi/src/syscall.rs` issues ToyOS's `syscall` on every OS. A program
reaches `filepicker` only through the `filepicker` port, which
`filepicker-api` opens with `toyos::endow`, so off ToyOS a pick needs a
transport the same source carries.

**Exit:** no manifest names this file, and each app above opens its window and
works on each host. The gate reads the first; nobody but a person on each host
judges the second.
