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
AArch64 one step behind, on `issues/toyos-runs-on-arm64.md`'s track.

- **M2 — clang and lld as a package inside ToyOS.** clang, lld
  and their runtime built *for* ToyOS on the host and installed by
  `/system/bin/pkg`; `clang hello.c && ./a.out` works in the guest. *Exit*:
  the in-guest compile-and-run test passes.
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
- LLVM ships as a binary seed. C is accepted because the compiler is LLVM, in
  programs too, important ones included.
- cargo's C dependencies are accepted.
- Perl and Python come to ToyOS.
- Make it work, then optimise, then compare: no performance study now, and no
  comparison before a compilation inside ToyOS succeeds.

**To build** (2026-10-01), in an order that is open. Where a stage names
libc's state it is `userland/libc` at `15625e0cb`.
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
  3 of `issues/a-childs-end-is-an-event-and-a-parent-takes-its-children-down.md`
  builds none, because a descriptor is a handle and passes `FD_SETSIZE`; what
  `select` answers for such a descriptor is open.
- **libc gains what cargo's libraries call**, among them `setvbuf` and
  `socketpair`, which it does not define; a `realpath` that resolves, a file
  `mmap` and record locks
  (`issues/libc-refuses-what-toyos-cannot-yet-answer.md`); file
  identities
  (`issues/libc-stat-answers-one-serial-number-for-every-file.md`),
  served like the locks by fsd; and socket descriptors that are pollable and
  can be non-blocking
  (`issues/libc-close-of-a-socket-ends-the-process.md`). Read from each
  library's source and not run: SQLite's default VFS takes `fcntl` record
  locks and keys them on `st_dev` and `st_ino`; libgit2 opens every repository
  through `realpath` and reads packs through a file `mmap`; curl and libssh2
  wait on non-blocking sockets, curl's multi handle wakes through a
  `socketpair`, and its TLS key log sets its buffering through `setvbuf`.
- **cargo's eight C libraries are cross-built for ToyOS**: curl, libgit2,
  libssh2, OpenSSL, SQLite, nghttp2, zlib and blake3. Compiled and linked
  with the toolchain's clang against that libc's C sysroot, the sources of
  nghttp2, zlib and blake3 build with nothing undefined, and the other five
  stop on headers, types or functions libc lacks. `openssl-src` 300.6.1 knows
  no ToyOS target and refuses one it does not know (`src/lib.rs`), and
  `openssl-sys` is a `cfg(unix)` dependency of `curl-sys` and `libssh2-sys`,
  which ToyOS is not. Open until M4: whether cargo's OpenSSL is built for
  ToyOS through its Perl `Configure`, or cargo takes curl's rustls backend
  there.
- **The tools a self-build runs are built for ToyOS**: Perl and Python, ported
  to start a child by spawn and never by fork; brush as the POSIX `sh`
  (`issues/ninja-runs-every-command-through-a-bin-sh-toyos-does-not-have.md`);
  uutils; make; CMake, its libuv ported to spawn; and awk. Those written in C
  or C++ wait on stage 3 of the child-process track and on
  `issues/there-is-no-dev-null.md`. A search of Perl 5.44.0's
  sources for `posix_spawn` matches nothing. Open: which Python, which make and
  which awk.
- **`pkg` installs a toolchain**: an archive past 256 MiB, and links. It
  inflates a whole archive in memory under `MAX_INFLATED`, 256 MiB, and refuses
  every link (`userland/pkg/src/main.rs`, `archive.rs`). A toolchain's size is
  measured on a proxy, the Linux-host release
  `toolchain-linux-x86_64-48dd24f826263d6c`: stripped with `llvm-objcopy
  --strip-all`, its `librustc_driver` is 184.7 MB, its clang 122.8 MB and its
  lld 78.6 MB, beside 181.1 MB of `x86_64-unknown-toyos` libraries. And the
  loader finds a package's own libraries
  (`issues/a-package-cannot-ship-its-own-libraries.md`): rustc's
  launcher names `librustc_driver` as needed (`llvm-readobj --needed-libs`).
- **The toolchain is `pkg` packages, and the image carries none of it.** Open:
  how it is split into packages, and how a self-host test's guest reaches
  them.

**Blocked on other tracks.** M2 needs packages over HTTPS
(`issues/a-package-is-a-directory-under-apps-and-the-installer-is-a-program.md`)
and the network stack under it (`issues/the-lan-is-not-yet-production-grade.md`,
`issues/the-internet-clients-work-unchanged.md`), room for about a
gigabyte of toolchain, and threads and `mmap` mature enough for LLVM
(`issues/std-and-libc-drop-the-answer-thread-join-gives.md`).
M2 and M4 also need libc to start a child process
(`issues/a-childs-end-is-an-event-and-a-parent-takes-its-children-down.md`). M4 needs git in the guest, storage durable and fast
enough for an LLVM build tree
(`issues/storage-is-layers-and-a-role-is-a-filesystem.md`), and
memory beyond what 2 MiB process pages allow
(`issues/process-memory-is-2-mib-pages-and-that-caps-the-process-count.md`).

**No linker runs inside ToyOS** (owner, 2026-10-04: "Just get rid of toyos
ld"). `/system/bin/toyos-ld` went with its crate, its `[programs]` row and its
host tests, and nothing replaced it: a ToyOS process cannot link an object, and
the ToyOS-hosted rustc names no linker the guest has
(`issues/the-hosted-rustc-names-a-linker-toyos-does-not-have.md`). *Exit*: the
commit that lands M2's compile-and-run test, which links inside the guest.

**What stops M2: LLVM, clang and lld built for a ToyOS host**, in the order
each blocks the next, as
`issues/bootstrap-cannot-build-llvm-clang-and-lld-for-a-toyos-host.md`
measures it.
- Run: what `issues/libc-refuses-what-toyos-cannot-yet-answer.md` lists,
  `issues/libc-has-no-pread-or-pwrite.md`, and
  `issues/libc-stat-answers-one-serial-number-for-every-file.md`.

**What M3 adds: a rustc that carries that LLVM**, after all of M2's.
- Build: `issues/rustc-llvm-cannot-build-for-a-toyos-host.md`.
- Link: `issues/a-rust-std-binary-cannot-link-the-cxx-runtime.md`.
- Test: `issues/a-worktree-cannot-build-a-hosted-rustc-of-its-own.md`.

M3's exit then waits on a linker in the guest
(`issues/the-hosted-rustc-names-a-linker-toyos-does-not-have.md`).

**Also owed, by milestone.**
- M2: the host triple a ToyOS-hosted LLVM records
  (`issues/a-toyos-hosted-llvm-is-configured-as-running-on-the-build-machine.md`,
  whose other half, the configure inside ToyOS, is M5's).
- M3: `issues/the-hosted-rustcs-stage2-carries-seventeen-proc-macro-libraries-no-image-needs.md`.
- M4: cargo's file locks,
  `issues/std-file-lock-answers-ok-and-locks-nothing.md`.
- M5: `issues/n2-does-not-compile-for-toyos.md`.
- The tools of "To build", the first programs that start a script by its
  path: `issues/nothing-launches-a-script-by-its-interpreter-line.md`.
