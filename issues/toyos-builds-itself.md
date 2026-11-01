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
library stays `sdk/libc`, ours. Each stage lands on x86-64 first and on
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

**Decided** (owner, 2026-10-09). M2 installs clang and lld from a local file
first, without waiting for packages over HTTPS, and links them statically:
"Yes, decoupled and static (Recommended)".

## From today to M4

The one list of what self-hosting still needs, in dependency order: an item
needs only items above it, and names them. An item points at the issue that
holds its facts; where the design is not yet in the tree, the item says so.
Status, as of 2026-10-22:
- **done**;
- **in progress**: built in part, on `main` or in an open pull request;
- **direction set**: the owner ruled what it must do, and its design is
  engineering's, not yet written in the tree or not yet reviewed;
- **not started**.

An owner is an issue under `issues/`, or this track.

**M2: clang and lld installed and run.**

| # | Item | Status | Owner | Blocked on |
|---|---|---|---|---|
| 1 | clang and `ld.lld` built for ToyOS, static, run in a guest (`hosted_clang_hello`) | done | this track (`build/hostedclang.rs`) | — |
| 2 | C reaches every file a process holds: descriptors (#975, #978) and served files through `toyos::files` (#988) | done | this track | — |
| 3 | `pkg` installs a toolchain: a streamed archive bounded by its volume's room, and links. #970 installs the clang package (83.8 MB archive, 279 MB installed) on a 1 GiB DATA in QEMU | in progress | `a-package-is-a-directory-under-apps-and-the-installer-is-a-program` | — |
| 4 | Room for a toolchain on DATA, durable. DATA serves `/apps`, `/config`, `/home` and `/state`; the full toolchain is about 1 GB | in progress | `storage-is-layers-and-a-role-is-a-filesystem` | — |
| 5 | User memory without physical contiguity. clang's driver, spawning `cc1` from `/apps`, is refused `ENOMEM` (measured), where a small parent spawns the same images. The likely refusal is the image's memory object, which `SYS_SHM_CREATE` allocates physically contiguous (`kernel/src/object/shm.rs`), about 148 MB for clang; that it is this allocation and not the spawn's own is inferred. Memory stage 2 makes a shared memory object scattered 4 KiB frames; `hosted_clang_hello` is the check that it was this | in progress | `process-memory-in-4-kib-pages-still-copies-each-programs-text` | — |
| 6 | **M2 exit**: a `pkg`-installed clang compiles, links and runs `hello.c` (#970's guest row, which compiles and stops at the link on 5), as `hosted_clang_hello` did with a clang staged on ROOT | in progress | this track, `toyos-has-no-c-compiler-inside-it` | 2–5 |

**M3: an LLVM-backed rustc.**

| # | Item | Status | Owner | Blocked on |
|---|---|---|---|---|
| 7 | Threads mature enough for LLVM | not started | `std-and-libc-drop-the-answer-thread-join-gives` | — |
| 8 | Concurrent reads and writes of one file: `pread` and `pwrite`, on a served file and a kernel one, which never move a shared offset (`tests/corpus/tinycc/214_at_calls.c`) | done | this track | — |
| 9 | LLVM, clang and lld built by bootstrap for a ToyOS host, and the libc names they call | not started | `bootstrap-cannot-build-llvm-clang-and-lld-for-a-toyos-host`, `rustc-llvm-cannot-build-for-a-toyos-host`, `libc-lacks-names-llvm-for-a-toyos-host-calls`, `a-toyos-hosted-llvm-is-configured-as-running-on-the-build-machine` | — |
| 10 | A std binary links the C++ runtime | not started | `a-rust-std-binary-cannot-link-the-cxx-runtime` | 9 |
| 11 | A package ships its own libraries (`librustc_driver`); moot for M2's static PIEs | not started | `a-package-cannot-ship-its-own-libraries` | 3 |
| 12 | **M3 exit**: the hosted rustc, which runs LLVM on several threads, builds and runs a program in the guest, linking with the guest's `ld.lld` | not started | `nothing-builds-the-toyos-hosted-rustc`, `the-hosted-rustc-names-a-linker-toyos-does-not-have`, `the-hosted-rustcs-stage2-carries-seventeen-proc-macro-libraries-no-image-needs` | 6–11 |

**M4: cargo, rustc and clang build ToyOS in the guest, byte-identical.**

| # | Item | Status | Owner | Blocked on |
|---|---|---|---|---|
| 13 | Process memory: 4 KiB pages, a ledger, demand paging, one mapping inventory, then private file mappings and coherent shared ones (owner direction, 2026-10-21; the design is out of the tree, under review) | direction set | `process-memory-in-4-kib-pages-still-copies-each-programs-text`, `no-physical-memory-fairness` | engineering: the design, with a wait and progress design for mapped pages without an automatic hang restart (owner, 2026-10-21) |
| 14 | Record locks the file server serves, and std's and cargo's locks on them | direction set | `std-file-lock-answers-ok-and-locks-nothing`, `libc-refuses-what-toyos-cannot-yet-answer` | engineering: the lock design, part of 13's |
| 15 | An unlinked open file lives until its last close or unmap | direction set | `an-unlinked-open-file-answers-stale` | 13 for the mapping |
| 16 | An exclusive create on `/tmp` (`O_CREAT\|O_EXCL`, `create_new`): the kernel's `OpenFlags::EXCLUSIVE` | done | this track | — |
| 17 | libc tells the truth: `sigaction` refuses a handler for a signal nothing raises, `st_nlink` is 1 and `strerror` names each code (done), and the user and group ids are the process's own | in progress | `libc-answers-uid-0-for-every-process` | — |
| 18 | libc gains `select`, `setvbuf` and `socketpair`. A descriptor is a small table number since #978, so `FD_SETSIZE` holds | not started | this track | — |
| 19 | The `libc` crate gains a ToyOS module, checked by `ctest` against `sdk/libc`'s headers | not started | this track | 18 |
| 20 | Children: `SIGCHLD`, sessions, a quit notice and Ctrl+C, the process track's stages 4–7 | in progress | `a-childs-end-is-an-event-and-a-parent-takes-its-children-down` | — |
| 21 | Sockets shared by their holders and inherited by a child, which curl, libssh2 and every inheriting server need | direction set | `a-socket-libc-or-socket2-has-not-bound-is-not-shared-with-a-child`, `libc-close-of-a-socket-is-unproven-on-a-connected-socket` | engineering: streams, listeners and datagram sockets shared; unbound C sockets and inheritance next |
| 22 | cargo's eight C libraries cross-built: curl, libgit2, libssh2, OpenSSL, SQLite, nghttp2, zlib, blake3; five stopped on headers, types and functions libc lacked at `15625e0cb` | not started | this track | 18, 19 |
| 23 | cargo's TLS: OpenSSL through its Perl `Configure`, or curl's rustls backend on `ToyOSOrg/ring`. A build from vendored sources needs neither, and neither needs the `toyos-crypto` track | not started, choice open | this track, `the-internet-clients-work-unchanged` | 22; HTTPS on `the-lan-is-not-yet-production-grade` |
| 24 | cargo at run time: its SQLite and its locks, sockets and children | not started | this track, `sqlite-and-postgresql-run-on-toyos` | 14–17, 20–22 |
| 25 | The tools a build runs: Perl and Python started by spawn, brush as `sh`, uutils, make, CMake with libuv on spawn, awk, and a script started by its interpreter line | not started | `ninja-runs-every-command-through-a-bin-sh-toyos-does-not-have`, `nothing-launches-a-script-by-its-interpreter-line` | 20 for the C tools |
| 26 | git in the guest | not started | this track | 22 |
| 27 | The toolchain as `pkg` packages, none of it in the image. #970 makes the clang package; how the rest splits, and how a self-host test's guest reaches them, is open | in progress | this track | 3, 11 |
| 28 | Storage durable and fast enough for an LLVM build tree, unmeasured | not started | `storage-is-layers-and-a-role-is-a-filesystem`, `an-fsync-of-a-served-file-syncs-its-whole-volume` | 4 |
| 29 | A build reproduces: an image's GUIDs from the commit, and one checkout building one kernel | not started | `an-images-guids-are-drawn-from-the-hosts-random-source`, `one-checkout-of-one-tree-builds-two-kernels` | — |
| 30 | **M4 exit**: the guest builds a userland program and then the kernel, and the hashes equal the host build's of the same commit | not started | this track | 12–29 |

What the items rest on. The compiles were made against libc at `15625e0cb`
(2026-10-01), before #975, #978 and #988 changed it:
- **3, 27, measured:** the old `pkg` inflated a whole archive in memory under
  `MAX_INFLATED`, 256 MiB, and refused every link (`system/pkg/src/main.rs`,
  `archive.rs`), which #970 replaces. Measured on the Linux-host release
  `toolchain-linux-x86_64-48dd24f826263d6c`, stripped: `librustc_driver`
  184.7 MB, clang 122.8 MB, lld 78.6 MB, and 181.1 MB of
  `x86_64-unknown-toyos` libraries.
- **11, read:** rustc's launcher names `librustc_driver` as needed
  (`llvm-readobj --needed-libs`).
- **18, read:** curl 8.21.0, which `curl-sys` 0.4.90 builds, does not compile
  without `select` (`lib/curlx/wait.c`), and waits through it, since `curl-sys`
  does not define `HAVE_POLL`. curl's multi handle wakes through a
  `socketpair`, and its TLS key log sets its buffering through `setvbuf`.
- **19, read:** `libc` 0.2.189 has no ToyOS module (`rg -i toyos` over `src`)
  and is empty for an OS it does not know; `ToyOSOrg/libc` does not exist.
  cargo's `curl-sys`, `libssh2-sys` and `libgit2-sys` import its C types
  unconditionally.
- **22, compiled at `15625e0cb`:** nghttp2, zlib and blake3 build with nothing
  undefined; curl, libgit2, libssh2, OpenSSL and SQLite stop on headers, types
  or functions libc lacked.
- **22 and 24, read:** SQLite's unix VFS takes `fcntl` record locks keyed on
  `st_dev` and `st_ino`; libgit2 opens every repository through `realpath` and
  reads packs through a file `mmap`; curl and libssh2 wait on non-blocking
  sockets. `openssl-src` 300.6.1 refuses a target it does not know
  (`src/lib.rs`), and `openssl-sys` is a `cfg(unix)` dependency of `curl-sys`
  and `libssh2-sys`, which ToyOS is not. That cargo bundles SQLite is read from
  its dependencies, not run.
- **25, read:** Perl 5.44.0's sources never call `posix_spawn`. Which Python,
  which make and which awk is open.

Owed by the `clang` package (item 3), and needed by no exit:
- **Licences.** The package carries LLVM's `LICENSE.TXT` and none of the
  licences of what `libtoyos_c.a` links besides `sdk/libc`, though
  `build/hostedclang.rs` packs `sysroot/lib`; a package published beyond a
  test's ROOT owes them.
- **A command by its name.** A shell finds no installed package's command by
  name: its `PATH` is `/system/bin` (`system/shell/src/main.rs`), so
  `clang hello.c` is typed `/apps/clang/bin/clang hello.c`. How an installed
  command is reached by its name is open.

Beside the path, and needed by none of its exits:
- **The boot handoff** (owner, 2026-10-21) removes the kernel's ROOT mount: the
  loader hands the supervisor and the system file server over as signed
  payloads, and `/system` goes onto the file protocol
  (`the-kernel-is-small-interrupts-post-and-threads-wait`, step 9).
- **Paths on `/tmp` resolve by the kernel's rules**
  (`the-kernel-resolves-a-path-by-rules-of-its-own`); a served path resolves
  by POSIX's (#988).
- **libc++'s `std::filesystem`** is proven in no guest, and its `remove_all`
  walks by path where a directory handle (`openat`, `unlinkat`) would not
  follow a link swapped in (`no-guest-case-reads-std-filesystem`,
  `remove-all-follows-a-link-swapped-in-mid-walk`). clang, lld and rustc
  call neither, so no exit waits on them; a C++ program that does is
  affected.
- **M5:** `n2-does-not-compile-for-toyos`, and Python and CMake for ToyOS.
- Every AArch64 stage lands one step behind x86-64, on `toyos-runs-on-arm64`.

**No image ships a linker** (owner, 2026-10-04: "Just get rid of toyos ld").
Until item 6 lands, `ld.lld` links inside ToyOS only where `hosted_clang_hello`
stages it.
