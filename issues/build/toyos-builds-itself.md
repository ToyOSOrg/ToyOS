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

**Decided** (owner, 2026-10-01).
- LLVM ships as a binary seed. C and C++ are accepted because the compiler is
  LLVM, in programs too, important ones included.
- cargo keeps its eight C libraries — curl, libgit2, libssh2, OpenSSL, SQLite,
  nghttp2, zlib and blake3 — and nothing replaces them with Rust.
- Perl and Python come to ToyOS.
- The host builds with the cargo rustup ships. #629, open, builds the fork's
  own for the host and is reworked to keep rustup's.
- Make it work, then optimise, then compare: no performance study now, and no
  comparison before a compilation inside ToyOS succeeds.

**The plan's stages** (2026-10-01), behind three open pull requests that land
first and in this order: #650, libc's POSIX surface for LLVM; #659, stage 4 of
the child-process track; #661, LLVM, clang and lld built for a ToyOS host as
far as libc lets them. The plan sets no order among the stages. Where a stage
names libc's state it is #650's at `15625e0cb`, which `main` does not have,
unless it says `main`.
- **The `libc` crate gains a ToyOS module**, checked by `ctest` against
  `userland/libc`'s headers. `libc` 0.2.189 has none (`rg -i toyos` over its
  `src` matches nothing) and is empty for an OS it does not know
  (`src/lib.rs`), and `ToyOSOrg/libc` does not exist (GitHub's API answers
  404). cargo's `curl-sys`, `libssh2-sys` and `libgit2-sys` import its C types
  unconditionally.
- **libc gains `select`.** curl 8.21.0, which `curl-sys` 0.4.90 builds, does
  not compile without it (`lib/curlx/wait.c`: `#error "We cannot compile
  without select() support."`) and waits on its sockets through it: `select.c`
  calls `poll` only under `HAVE_POLL`, which `curl-sys` does not define. Stage
  3 of `issues/kernel/a-childs-end-is-an-event-and-a-parent-takes-its-children-down.md`
  builds none, because a descriptor is a handle and passes `FD_SETSIZE`; what
  `select` answers for such a descriptor is open.
- **libc gains what cargo's libraries call**, among them `flock`, a `realpath`
  that resolves, a file `mmap`, record locks and file identities served by fsd,
  socket descriptors that are pollable and can be non-blocking, `setvbuf` and
  `socketpair`. Read from each library's source and not run: SQLite's default
  VFS takes `fcntl` record locks and keys them on `st_dev` and `st_ino`;
  libgit2 opens every repository through `realpath` and reads packs through a
  file `mmap`; curl and libssh2 wait on non-blocking sockets, and curl's multi
  handle wakes through a `socketpair`. `main`'s libc defines no `flock`,
  `realpath`, `setvbuf` or `socketpair`; #650's refuses `realpath`, a file
  `mmap` and a record lock, and files that as a defect.
- **cargo's eight C libraries are cross-built for ToyOS.** Compiled and linked
  with the toolchain's clang against #650's C sysroot, the sources of nghttp2,
  zlib and blake3 build with nothing undefined, and the other five stop on
  headers, types or functions libc lacks. `openssl-src` 300.6.1 knows no ToyOS
  target and refuses one it does not know (`src/lib.rs`), and `openssl-sys` is
  a `cfg(unix)` dependency of `curl-sys` and `libssh2-sys`, which ToyOS is not.
  Open until M4: whether cargo's OpenSSL is built through its Perl `Configure`,
  or cargo takes curl's rustls backend.
- **The tools a self-build runs are built for ToyOS**: Perl and Python, ported
  to start a child by spawn and never by fork; brush as the POSIX `sh`
  (`issues/build/ninja-runs-every-command-through-a-bin-sh-toyos-does-not-have.md`);
  uutils; make; CMake, its libuv ported to spawn; and awk. They wait on stage 3
  of the child-process track and on `issues/filesystem/there-is-no-dev-null.md`.
  A search of Perl 5.44.0's sources for `posix_spawn` matches nothing. Which
  Python, which make and which awk the plan does not say.
- **`pkg` installs a toolchain**: an archive past 256 MiB, and links. It
  inflates a whole archive in memory under `MAX_INFLATED`, 256 MiB, and refuses
  every link (`userland/pkg/src/main.rs`, `archive.rs`). A toolchain's size is
  measured on a proxy, the Linux-host release
  `toolchain-linux-x86_64-48dd24f826263d6c`: stripped with `llvm-objcopy
  --strip-all`, its `librustc_driver` is 184.7 MB, its clang 122.8 MB and its
  lld 78.6 MB, beside 181.1 MB of `x86_64-unknown-toyos` libraries. And the
  loader finds a package's own libraries
  (`issues/filesystem/a-package-cannot-ship-its-own-libraries.md`): rustc's
  launcher names `librustc_driver` as needed (`llvm-readobj --needed-libs`).
- **The toolchain is `pkg` packages, and the image carries none of it.** How it
  is split into packages, and how a self-host test's guest reaches them, the
  plan does not say.

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
  `issues/build/libc-headers-are-written-by-hand-and-drift-from-its-definitions.md`,
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
