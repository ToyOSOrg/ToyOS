---
status: open
kind: tooling
opened: 2026-09-29
---

# The build runs C and C++ tools that ToyOS does not yet build and run

Every tool `main`'s build, tests, workflows and metal loop run outside Rust and
QEMU, found by the ways `.claude/agents/reviewer.md`'s Arrivals names, with the
verdict of `CLAUDE.md`'s Dependencies on each; a refused row is a standing
failure. A workflow's package manager, `apt-get` or `brew`, is how a row
arrives and is not one. M4 and M5 are stages of `issues/build/toyos-builds-itself.md`.

| tool | runs | verdict | exit |
|---|---|---|---|
| Python | LLVM's CMake, whenever this host builds an LLVM (`src/llvm.rs`) | admitted: no Rust tool does the job, LLVM's CMake requires one (`find_package(Python3 … REQUIRED)` in `rust/src/llvm-project/llvm/CMakeLists.txt`) | M5 runs it in the guest |
| CMake | rustc's bootstrap, for LLVM and clang; `src/llvm.rs`, for the LLVM's key | admitted: no Rust tool does the job, LLVM, clang and LLD are described in CMake, and upstream's only other descriptions are a GN overlay it does not support and a Bazel one | M5 runs it in the guest |
| Ninja | runs the build CMake generates for LLVM | admitted: no Rust tool does the job, bootstrap has CMake generate for Ninja, and no CMake generator targets a Rust tool | M5 runs it in the guest |
| `git` | every build, which also adds worktrees (`src/worktree.rs`) and updates submodules (`src/lib.rs`, `src/sysroot.rs`); rustc's bootstrap; every workflow's checkout | admitted: no Rust tool does the job, gitoxide 0.85 adds no worktree and updates no submodule | M4 runs it in the guest |
| `cc`, `c++` and `ar` on a Linux host, `build-essential` on the nightly's runners | rustc links every host binary through `cc`; `cc` and `c++` compile LLVM, clang, LLD and `rustc_llvm` (`src/llvm.rs` names both to bootstrap) and `ring`'s C for `tests/https-server-host` and `tests/https-fetch-host`; `ar` archives what `cc::Build` compiles | admitted: no Rust tool compiles C or C++, or takes rustc's host link | M5: no host in the loop |
| `ovmf-generic` | the UEFI firmware of the nightly's guest containers (`src/firmware.rs`), packaged by Debian apart from QEMU | admitted: QEMU's own firmware, and no Rust firmware does its job | the instrument's QEMU carries its own firmware |
| `ca-certificates` | the trust store `git` and `curl` verify against in the nightly's containers | admitted: data both of them need | goes when neither runs there |
| `env` | the two portability jobs run `cargo run -- --build-only` under it, with `GITHUB_ACTIONS` and `CI` unset | admitted: no Rust tool on the runner unsets a variable for one command | M5: no host in the loop |
| `sudo`, `dd`, `wipefs`, `efibootmgr`, `udevadm`, `mount`, `umount`, `reboot`, `visudo`, `install`, `systemd-analyze`, `ip`, `date`, `cat`, `ls`, `test`, `mkdir`, `rm`, `true`, and the login shell each command reaches | `src/metal.rs`, over `ssh`, on the T14's Ubuntu and never on a development host | admitted: no Rust tool does their job on that Ubuntu | Ubuntu leaves the metal loop (`issues/boot-media/the-machine-updates-itself-without-ubuntu.md`) |
| `sh` running `rust/x`, and Python running `x.py` and `bootstrap.py` | every toolchain build (`src/toolchain.rs`) | refused: a Rust tool does it, upstream's bootstrap binary, which builds with stable cargo, fetches its own stage0 (`rust/src/bootstrap/src/core/download.rs:595`) and needs no Python | `src/toolchain.rs` runs the bootstrap binary |
| `curl` in rustc's bootstrap | fetches the stage0 `rust/src/stage0` pins, for a compiler or LLVM build whose build directory lacks it, whichever bootstrap runs | refused: a Rust tool does it, rustup installs the dated beta the pin names, and bootstrap takes a stage0 through `build.rustc` and `build.cargo`, as `src/sysroot.rs` hands it one | no toolchain build fetches with `curl` |
| `curl` in `src/release.rs` and `src/sdkversion.rs` | the toolchain release's lookup and download and the crates.io index, on CI runners only | refused: a Rust tool does it, `ureq`, which `userland/doom/build.rs` already fetches with | those fetches are Rust's |
| `tar` and `zstd` | `src/release.rs` packs and unpacks the toolchain release, on CI runners only | refused: a Rust tool does it, the `tar` crate `userland/doom/build.rs` already unpacks with, and a zstd crate | both are done in Rust, in-process |
| `gh` | `src/release.rs` asks whether a toolchain release exists, creates it and moves the `sdk-<version>` alias, on the nightly's `build` runner | refused: not C or C++ source, it is Go | `src/release.rs` speaks GitHub's REST API itself |
| `ssh` | `src/metal.rs` reaches the T14's Ubuntu with it, only in the metal loop | refused: a Rust tool does it, the repository's own russh client `crate::build::ssh_client_host`, which `src/metaltalk.rs` already drives | `src/metal.rs` drives that client, or Ubuntu leaves the loop |
| `cc`, `c++`, `ar` and `xcrun` on a macOS host, Apple's Command Line Tools | what the Linux row's tools do, and rustc asks `xcrun` for the SDK on every host link that names no `SDKROOT` (`rust/compiler/rustc_codegen_ssa/src/back/apple.rs:230`), as `src/llvm.rs` does for the LLVM's key | refused: one host OS alone | M5: no host in the loop |
| `newfs_msdos` and `hdiutil` | `toyos-fat32`'s host tests format and mount their fixtures with them (`toyos-fat32/tests/common/mod.rs`), which keeps the `host` job on `macos-latest` | refused: one host OS alone | `issues/filesystem/fat32-suite-needs-macos-binaries.md` |
| `diag/flash.sh` | the owner's flash of a stick by hand: `bash`, and the `stat`, `seq`, `tr`, `grep`, `cut` and `sync` it strings together | refused: shell of our own | `issues/build/the-owners-flash-script-runs-diskutil.md` |
| `diskutil` and `plutil` | `diag/flash.sh`, and `diskutil` in the README's flashing steps | refused: one host OS alone | `issues/build/the-owners-flash-script-runs-diskutil.md` |
| `shasum`, a Perl script on macOS | `diag/flash.sh` hashes the image with it | refused: a Rust tool does it, `sha2`, which `src/release.rs` hashes with | `issues/build/the-owners-flash-script-runs-diskutil.md` |
| `dd` | `diag/flash.sh` writes the stick with it | refused: a Rust tool does it, the build system can write the image itself | `issues/build/the-owners-flash-script-runs-diskutil.md` |
| `sudo` | `diag/flash.sh` runs `dd` under it | admitted: no Rust tool raises a process to root on macOS | goes with `diag/flash.sh` |
| `sh` running rustup's `rustup-init.sh` | the nightly's three rustup installs, fetched with `curl` | refused: a Rust tool does it, rustup's own `rustup-init` binary | a job installs rustup without it |
| `nightly.yml`, job `build`, step "disk, QEMU, CMake and Ninja" | `sudo rm -rf` of five preinstalled SDK directories, then `sudo apt-get update` and `install` | refused: shell of our own | each step is one command |
| `nightly.yml`, step `deps`, which jobs `guest` and `tcg` share | the snapshot archive read out of `debian.sources` with `sed`, checked with `test` and written back with `echo` and `rm`, a three-attempt `apt-get` loop with `sleep` and `cat`, `git config`, and rustup through `curl`, `sh` and `echo` | refused: shell of our own | each step is one command |
| `nightly.yml`, job `portability-linux`, step "deps" | the same loop, `git config`, and rustup the same way | refused: shell of our own | each step is one command |
| `nightly.yml`, job `portability-macos`, the rustup step | `curl`, `sh rustup-init.sh`, and `echo` into `$GITHUB_PATH` | refused: shell of our own | each step is one command |
| `umask 077 && cat > <staged>` | `src/metal.rs` stages the sudoers rule on the T14 with it | refused: shell of our own | Ubuntu leaves the metal loop |
