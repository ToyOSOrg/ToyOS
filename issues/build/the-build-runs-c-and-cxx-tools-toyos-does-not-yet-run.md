---
status: open
kind: tooling
opened: 2026-09-29
---

# The build runs C and C++ tools that ToyOS does not yet build and run

Each is declared by `check_prerequisites` in `src/main.rs` — `REQUIRED`, which
exits, and `ALSO_USED`, which names what is absent and continues — and in the
README's Prerequisites. None reaches a guest. **Exit, for each of the four
below**: it builds and runs on ToyOS, on the self-hosting track
(`issues/build/toyos-builds-itself.md`).

- **Python.** `rust/x` is a `/bin/sh` script whose whole job is to find one of
  `python3 python py python2 uv` and exec `x.py`, which ends in
  `rust/src/bootstrap/bootstrap.py`; `src/toolchain.rs` picks `./x` when it
  exists. So a clean clone cannot build a toolchain without Python 3. The
  preflight looks for any of those names because that is what `rust/x` searches,
  and scans `PATH` rather than running `--version` because asking macOS for `py`
  opens the Command Line Tools installer. No Rust tool does the job: upstream
  has no Python-free entry (`x`, `x.py` and `src/tools/x` all end in
  `bootstrap.py`); `src/bootstrap` builds with rustup's stable cargo and no
  Python, but running it in place of `bootstrap.py` has `src/toolchain.rs` take
  over `bootstrap.py`'s environment contract at every fork bump; and LLVM's own
  CMake requires a Python 3 (`find_package(Python3 … REQUIRED)`) whichever
  starts the build.
- **`cc`** has two jobs. It links every host binary — the build system, the
  harness, `toyos-ld`, rustc stage2, clang — and rustc sets `SDKROOT` for it;
  no guest binary links through it, every one links through the toolchain's
  `rust-lld`. And it is the C++ compiler of LLVM, clang, LLD and `rustc_llvm`
  (`bootstrap.toml`'s `cc`/`cxx`, named in `src/llvm.rs`, with `xcrun` asked for
  the SDK), and compiles `ring`'s C for `tests/https-server-host` and
  `tests/https-fetch-host`. No Rust tool does the job: `rust-lld` can take only
  the link, and nothing replaces the compile but a clang the host did not build.
- **CMake and Ninja.** rustc's bootstrap builds LLVM and clang from
  `ToyOSOrg/llvm-project` with CMake driving Ninja, whenever this host has not
  built that LLVM. CMake is in `REQUIRED` because every build keys the host's
  LLVM on `cmake --version` (`src/llvm.rs`); Ninja is in `ALSO_USED` because a
  build whose LLVM is already built does not run it. No Rust tool does the job:
  LLVM, clang and LLD are described in CMake, and the only other descriptions
  upstream carries are an unsupported GN overlay (`BUILD.gn`) and a Bazel one.

**Also run, and declared here:**

- **`git`** — `REQUIRED`: every build, and the image ships what git says is
  tracked.
- **`gh`** — Go, so outside the rule. `src/release.rs` asks GitHub whether a
  toolchain release exists, creates it and moves the `sdk-<version>` alias with
  it, on a runner only. Exit: `src/release.rs` speaks GitHub's REST API itself.
- **`curl`**, and **`ca-certificates`**, the trust store it verifies against —
  `src/release.rs` downloads the toolchain release, `src/sdkversion.rs` asks
  the crates.io index, and the nightly's container and macOS jobs fetch
  `rustup-init.sh` with it. Exit: those fetches are Rust's, and a job
  installs rustup without it.
- **`tar`** and **`zstd`** — `src/release.rs` packs and unpacks the toolchain
  release. Exit: both are done in Rust, in-process.
- **`ssh`** — `src/metal.rs` reaches the T14's Ubuntu with it, only when the
  metal loop is asked for. Exit: that loop drives the repository's own russh
  client, `crate::build::ssh_client_host`, which `src/metaltalk.rs` already
  drives at a booted T14.
- **`ovmf-generic`** — the guest container's UEFI firmware
  (`src/firmware.rs`), which Debian packages apart from QEMU. Exit: the
  instrument's QEMU carries its own firmware.
- **`build-essential`** — Debian's package for `cc` and `c++`, which the
  nightly's guest containers and `portability-linux` install: every host
  binary those jobs build links through `cc`, `ring`'s C compiles with it, and
  `portability-linux` builds LLVM with `c++`. Exit: it goes with `cc`.
