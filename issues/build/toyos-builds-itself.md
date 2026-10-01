---
status: open
kind: track
opened: 2026-09-27
---

# ToyOS builds itself

The north star: ToyOS rebuilds its own sources inside ToyOS and reproduces the
bytes the host built. A bootstrap from source with no binary seed is out of
scope (owner, 2026-09-27). The compiler is LLVM throughout: rustc's, and clang
with lld, one build of one fork, `ToyOSOrg/llvm-project`. The C
library stays `userland/libc`, ours. Each stage lands on x86-64 first and on
AArch64 one step behind, on `issues/kernel/toyos-runs-on-arm64.md`'s track.

- **M2 — clang and lld as a package inside ToyOS; toyos-ld gone.** clang, lld
  and their runtime built *for* ToyOS on the host and installed by
  `/system/bin/pkg`; `clang hello.c && ./a.out` works in the guest. *Exit*:
  the in-guest compile-and-run test passes, and toyos-ld — today the only
  linker a ToyOS process can run — is deleted with its crate, its
  `[programs]` row and its tests.
- **M3 — an LLVM-backed rustc inside ToyOS.** The
  hosted rustc carries LLVM instead of Cranelift. *Exit*: a
  Rust program compiled and run inside ToyOS by the hosted rustc.
- **M4 — ToyOS builds ToyOS byte-identical to the host.** cargo, rustc and
  clang in the guest build a userland program and then the kernel, and the
  bytes equal the host's. *Exit*: a guest build of the image whose hashes match
  the host build of the same commit.
- **M5 — ToyOS rebuilds its own compilers to a fixed point; the host is no
  longer needed.** The guest's toolchain builds the next toolchain, its LLVM
  with Python and CMake built for ToyOS, and that one builds itself
  again to the same bytes.
  *Exit*: the fixed point, reached with no host in the loop.

**Blocked on other tracks.** M2 needs packages over HTTPS
(`issues/filesystem/a-package-is-a-directory-under-apps-and-the-installer-is-a-program.md`)
and the network stack under it (`issues/hardware/the-lan-is-not-yet-production-grade.md`,
`issues/design-debt/the-internet-clients-work-unchanged.md`), room for about a
gigabyte of toolchain, and threads and `mmap` mature enough for LLVM
(`issues/kernel/std-and-libc-drop-the-answer-thread-join-gives.md`).
M2 and M4 also need libc to start a child process
(`issues/kernel/a-childs-end-is-an-event-and-a-parent-takes-its-children-down.md`). M4 needs git in the guest, storage durable and fast
enough for an LLVM build tree
(`issues/filesystem/storage-is-layers-and-a-role-is-a-filesystem.md`), and
memory beyond what 2 MiB process pages allow
(`issues/kernel/process-memory-is-2-mib-pages-and-that-caps-the-process-count.md`).

**What M2 must do to delete toyos-ld.** It is frozen and links nothing the host
builds; what keeps it is that it is the one linker a ToyOS process can run,
shipped as `/system/bin/toyos-ld` by `system.toml`'s `[programs]` row and named
by the ToyOS-hosted rustc
(`issues/build/the-hosted-rustc-names-a-linker-toyos-does-not-have.md`). It
goes when lld runs in the guest: the row, the crate and its host tests go together, the hosted rustc names `rust-lld`, and
the published crates.io crate is yanked.

**What stops M2: LLVM, clang and lld built for a ToyOS host**, in the order
each blocks the next.
- Configure: the `clang-tblgen` and the CMake system of
  `issues/build/bootstrap-cannot-build-llvm-clang-and-lld-for-a-toyos-host.md`,
  and `issues/build/the-c-sysroot-has-no-libm-so-llvms-configure-fails.md`.
- Compile: `issues/build/toyos-libc-lacks-the-posix-surface-llvm-compiles-against.md`,
  with the names it leaves to
  the header gate (`toyos-libc-copies/src/prototypes.rs`),
  and the bootstrap issue's `bit.h` and `is_local_impl`.
- Link: the POSIX issue's functions, with those it leaves to the child-process
  track's stage 3 and to `issues/build/libc-has-no-alarm.md`.
- Run: `issues/build/libc-mmap-ignores-the-file-it-is-asked-to-map.md`,
  `issues/build/libc-fcntl-and-fchmod-answer-0-and-do-nothing.md`,
  `issues/build/libc-pread-and-pwrite-move-the-offset-another-thread-shares.md`
  and `issues/build/libc-readdir-calls-every-entry-a-regular-file.md`.

**What M3 adds: a rustc that carries that LLVM**, after all of M2's.
- Build: `issues/build/rustc-llvm-cannot-build-for-a-toyos-host.md`.
- Link: `issues/build/a-rust-std-binary-cannot-link-the-cxx-runtime.md` and
  `issues/build/a-rust-std-program-defines-no-aligned-alloc.md`.
- Test: `issues/build/a-worktree-cannot-build-a-hosted-rustc-of-its-own.md`.

M3's exit then waits on a linker in the guest
(`issues/build/the-hosted-rustc-names-a-linker-toyos-does-not-have.md`), which
toyos-ld is not: it refuses every executable with thread-local storage, so
every std program.
