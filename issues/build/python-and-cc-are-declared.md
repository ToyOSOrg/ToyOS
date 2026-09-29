---
status: open
kind: tooling
opened: 2026-08-08
---

# Every toolchain build runs Python, CMake and Ninja, and every host link runs `cc`

**The owner ruled on 2026-08-08: *"its required by rusts toolchain i guess we can
be transparent about that."*** Both are named in the README's Prerequisites
section and by `check_prerequisites` in `src/main.rs`, which is now two lists —
`REQUIRED`, which exits (`git`, `rustup`, `qemu-system-x86_64`, `cc`), and
`ALSO_USED`, which names what is absent and continues (a Python, `df`, `ps`,
`find`). The README's opening no longer claims Rust and QEMU are the whole
setup.

**The entry stays open because declaring is not removing.** The hole is the
same size: `bootstrap.py` still cannot run inside ToyOS, and the second option
below — a Rust bootstrap in the `rust/` fork — is still the only thing that
closes it.

Two details the fix had to get right. The preflight looks for *any* of
`python3 python py python2 uv`, because that is what `rust/x` searches and a
machine with only `python` builds fine; and it scans `PATH` rather than running
`--version`, because asking macOS for `py` opens the Command Line Tools
installer. `cc` is stated with its scope attached wherever it appears — no guest
binary links through it — because *"ToyOS needs a C compiler"* is false and
reads as a far larger claim than the truth.

This is the entry that says the two largest holes in *"Rust and QEMU, one
command"* are real, and what follows is the whole of both.

**The owner ruled on 2026-09-01: it stays declaration-only, and it stays
open.** Closing the Python half means a Rust bootstrap inside the `rust/` fork —
a large delta carried against upstream forever, for no product benefit today.
That is a real cost and this is a real hole, so the entry is not downgraded: it
remains a present-state weakness on the self-hosting track, to be sequenced when
that track is funded rather than taken opportunistically. Do not propose the
Rust bootstrap again as an incidental fix; do not soften the entry either.

`src/toolchain.rs:749` picks `./x` when `rust/x` exists, which it does. That file
is a `/bin/sh` script whose whole job is `SEARCH="python3 python py python2 uv"`,
and it execs `x.py` → `rust/src/bootstrap/bootstrap.py`. So a clean
clone cannot build a toolchain without Python 3. It is upstream's bootstrap and
not our code, which is why it is stated rather than blamed — but the bar has no
upstream exemption, and `bootstrap.py` can never run inside ToyOS.

Separately, and measured with `rustup run toyos rustc --print link-args` on a
trivial host binary: rustc invokes `"cc"` and sets
`SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk`. rustup installs
neither. Every *host* binary goes through it — the build system, the harness,
`toyos-ld`, rustc stage2, clang. **No guest binary does**: every guest binary
links through the toolchain's `rust-lld`, so nothing that boots is touched.

The cheap half of the fix — a preflight and a README that say what the machine
actually needs — is done. `REQUIRED` carries `cc` and `ALSO_USED` carries the
Python search list (`src/main.rs:18`, `:28`). The expensive half is untouched.

**CMake and Ninja joined the list on 2026-09-27, by the owner's ruling**: rustc's
LLVM is built from source, from `ToyOSOrg/llvm-project`, with clang beside it,
and rustc's bootstrap builds LLVM with CMake driving Ninja. Both are declared
where the others are — CMake in `REQUIRED` in `src/main.rs`, because every
build keys the host's LLVM on `cmake --version` (`src/llvm.rs`), and Ninja in
`ALSO_USED`, because a build whose LLVM this host has already built runs it
not — and in the README's Prerequisites. On macOS they come from Homebrew (`brew install cmake ninja`),
with no workaround; on the nightly's toolchain runner at the versions
`.github/workflows/nightly.yml` pins from Ubuntu 24.04's archive (`cmake
3.28.3-1build7`, `ninja-build 1.11.1-2`); the two portability jobs install
their platform's own, which is their premise. Neither reaches a guest. The
exit condition is Python's: they go when the build no longer needs a host,
which is the self-hosting track's last stage
(`issues/build/toyos-builds-itself.md`).

**What removing each takes.**

- **Python.** Upstream has no Python-free entry: `x`, `x.py` and
  `src/tools/x` all end in `bootstrap.py`. But `src/bootstrap` builds with
  rustup's stable cargo, `--locked`, and no Python, and the binary downloads
  its own stage0 (`download_beta_toolchain`) and looks for a Python only to run
  tests. So `src/toolchain.rs` could build and run it in place of
  `bootstrap.py`, with no change to the fork, taking over `bootstrap.py`'s
  environment contract at every fork bump. That removes Python only from a
  build that reuses a keyed LLVM: LLVM's own CMake requires a Python 3
  (`find_package(Python3 … REQUIRED)`), so building an LLVM needs one until CMake goes.
- **`cc` has two jobs**: it links every host binary, and it is
  the C++ compiler of LLVM, clang, LLD and `rustc_llvm` (`bootstrap.toml`'s
  `cc`/`cxx`, named in `src/llvm.rs`, with `xcrun` asked for the SDK). It also
  compiles `ring`'s C for `tests/https-server-host` and
  `tests/https-fetch-host`. `rust-lld` can take only the link. Nothing replaces the compile but a clang the host did not build.
- **CMake and Ninja.** LLVM, clang and LLD are described in CMake. The only other build
  descriptions upstream carries are an unsupported GN overlay (`BUILD.gn`)
  and a Bazel one. Replacing CMake means writing one of those and keeping it
  in Rust, which is M5's.
