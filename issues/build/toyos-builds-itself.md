---
status: open
kind: track
opened: 2026-09-27
---

# ToyOS builds itself

The north star: ToyOS rebuilds its own sources inside ToyOS and reproduces the
bytes the host built. A bootstrap from source with no binary seed is out of
scope (owner, 2026-09-27). The compiler is LLVM throughout: rustc's, and clang
with lld, one build of one fork, `ToyOSOrg/llvm-project` (`forks.toml`). The C
library stays `userland/libc`, ours. Each stage lands on x86-64 first and on
AArch64 one step behind, on `issues/kernel/toyos-runs-on-arm64.md`'s track.

- **M1 — clang cross-built, toyos-cc gone.** The host builds LLVM, clang and
  lld from the fork as part of the toolchain; a C program compiled by that
  clang against libc's C sysroot runs in QEMU, doomgeneric is built by it, and
  toyos-cc is deleted. *Exit*: `c_hello`, `doom_frames` and the C corpus green
  on clang, and toyos-cc's crate, tests and image row gone. Left for AArch64: clang's driver knows `aarch64-unknown-toyos`,
  an AArch64 userland build makes its C sysroot and doomgeneric compiles
  against it; no AArch64 C program has been linked and run.
- **M2 — clang and lld as a package inside ToyOS; toyos-ld gone.** clang, lld
  and their runtime built *for* ToyOS on the host and installed by
  `/system/bin/pkg`; `clang hello.c && ./a.out` works in the guest. *Exit*:
  the in-guest compile-and-run test passes, and toyos-ld — today the only
  linker a ToyOS process can run — is deleted with its crate, its
  `[programs]` row and its tests.
- **M3 — libc++, and an LLVM-backed rustc inside ToyOS.** libc++, libc++abi
  and libunwind for ToyOS; a C++ program with exceptions and threads runs; the
  hosted rustc carries LLVM instead of Cranelift. *Exit*: that C++ test, and a
  Rust program compiled and run inside ToyOS by the hosted rustc.
- **M4 — ToyOS builds ToyOS byte-identical to the host.** cargo, rustc and
  clang in the guest build a userland program and then the kernel, and the
  bytes equal the host's. *Exit*: a guest build of the image whose hashes match
  the host build of the same commit.
- **M5 — ToyOS rebuilds its own compilers to a fixed point; the host is no
  longer needed.** The guest's toolchain builds the next toolchain, and that
  one builds itself again to the same bytes. Python (`bootstrap.py`), CMake and
  Ninja leave the build (`issues/build/python-and-cc-are-declared.md`).
  *Exit*: the fixed point, reached with no host in the loop.

**Blocked on other tracks.** M2 needs packages over HTTPS
(`issues/filesystem/a-package-is-a-directory-under-apps-and-the-installer-is-a-program.md`)
and the network stack under it (`issues/hardware/the-lan-is-not-yet-production-grade.md`,
`issues/design-debt/the-internet-clients-work-unchanged.md`), room for about a
gigabyte of toolchain, and threads and `mmap` mature enough for LLVM
(`issues/kernel/std-and-libc-drop-the-answer-thread-join-gives.md`). M3 needs
thread-local `errno`, locale support or libc++'s no-localization build, and
`dl_iterate_phdr` in libc. M4 needs git in the guest, storage durable and fast
enough for an LLVM build tree
(`issues/filesystem/storage-is-layers-and-a-role-is-a-filesystem.md`), and
memory beyond what 2 MiB process pages allow
(`issues/kernel/process-memory-is-2-mib-pages-and-that-caps-the-process-count.md`).

**What M2 must do to delete toyos-ld.** It is frozen and links nothing the host
builds; what keeps it is that it is the one linker a ToyOS process can run,
shipped as `/system/bin/toyos-ld` by `system.toml`'s `[programs]` row and named
by the ToyOS-hosted rustc
(`issues/build/the-hosted-rustc-names-a-linker-toyos-does-not-have.md`). It
goes when lld runs in the guest: the row, the crate, its host tests and its
`src/sourcegate.rs` rows go together, the hosted rustc names `rust-lld`, and
the published crates.io crate is yanked.
