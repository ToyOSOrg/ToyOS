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

**The bar: Linux on this T14 building the same LLVM with a clang-built
clang+lld.** It is a recipe, and a ToyOS run matches every element of it:

- *Source*: `rust-lang/llvm-project` at
  `52ed14fcd56afc30f9cccd8ca8ce237c2eef7e04`.
- *Building compiler*: clang+lld at `52ed14fc`, Release (`-O3 -DNDEBUG`),
  `LLVM_ENABLE_LTO=OFF`, `LLVM_BUILD_INSTRUMENTED=OFF`, no profile data,
  built by clang+lld at `52ed14fc` with the same configuration: the `s2`
  lines below. Stage 1 is only how the first clang exists: gcc, which has
  no ToyOS target, builds it with the `s1` lines
  (`gcc (Ubuntu 13.3.0-6ubuntu2~24.04.1) 13.3.0`).
- *Build*: that clang+lld builds clang+lld from the same source with the
  `s3` lines below, configured afresh; cmake 3.28.3, ninja 1.11.1.
- *Timed span*: the last line's `ninja` alone.
- *Warm*: each span begins within 8 s of the end of another build of this
  source, the package at 68 to 75 °C and `File system inputs` 0.
- *Power envelope*: on AC; `platform_profile` `performance`; intel_pstate
  active, governor `powersave`, EPP `balance_performance`. RAPL through the
  MSR: PL1 64 W, PL2 64 W, peak 121 W. RAPL through MMIO: PL1 20 W, PL2
  64 W, peak 121 W. Both PL1 windows are 27983872 µs. All but the
  intel_pstate mode and the windows are read back every 60 s and at the
  span's start and end.
- *Validity*: a sample counts only if every read-back during its span
  matches the power envelope.
- *Machine*: i5-1135G7, 8 threads, 16476082176 B RAM; Ubuntu 24.04.4, kernel
  6.8.0-142-generic, ext4 on LVM.

The lines, run in bash, which splits `$CONF`:

```
W=$HOME/llvm-baseline; SHA=52ed14fcd56afc30f9cccd8ca8ce237c2eef7e04
mkdir -p $W; cd $W
git init -q src && git -C src remote add origin https://github.com/rust-lang/llvm-project.git
git -C src fetch -q --depth 1 origin $SHA && git -C src checkout -q FETCH_HEAD
CONF="-G Ninja -DCMAKE_BUILD_TYPE=Release -DLLVM_ENABLE_PROJECTS=clang;lld -DLLVM_TARGETS_TO_BUILD=X86;AArch64 -DLLVM_ENABLE_ASSERTIONS=OFF -DLLVM_INCLUDE_TESTS=OFF -DLLVM_INCLUDE_BENCHMARKS=OFF -DLLVM_INCLUDE_EXAMPLES=OFF"
rm -rf s1; cmake -S src/llvm -B s1 $CONF -DCMAKE_C_COMPILER=gcc -DCMAKE_CXX_COMPILER=g++ > s1-config.log
/usr/bin/time -v -o s1-time.txt ninja -C s1 -j8 clang lld > s1-build.log
rm -rf s2; PATH=$W/s1/bin:$PATH cmake -S src/llvm -B s2 $CONF -DCMAKE_C_COMPILER=$W/s1/bin/clang -DCMAKE_CXX_COMPILER=$W/s1/bin/clang++ -DLLVM_ENABLE_LLD=ON > s2-config.log
PATH=$W/s1/bin:$PATH /usr/bin/time -v -o s2-time.txt ninja -C s2 -j8 clang lld > s2-build.log
rm -rf s3; PATH=$W/s2/bin:$PATH cmake -S src/llvm -B s3 $CONF -DCMAKE_C_COMPILER=$W/s2/bin/clang -DCMAKE_CXX_COMPILER=$W/s2/bin/clang++ -DLLVM_ENABLE_LLD=ON > s3-config.log
PATH=$W/s2/bin:$PATH /usr/bin/time -v -o s3-time.txt ninja -C s3 -j8 clang lld > s3-build.log
```

The valid stage-3 samples: 41:47.28, 41:48.02 and 41:49.40; min 41:47.28,
median 41:48.02, max 41:49.40. For context only, the valid stage-2 samples,
the gcc-built clang+lld building the same source with the `s2` lines under
the same rule: 45:08.76, 45:08.78, 45:12.48 and 45:14.15; min 45:08.76,
median 45:10.63, max 45:14.15.

*Exit*: a ToyOS build of the same source with the `s3` configuration, by a
clang+lld built by the recipe above, on this T14, warm, and at the power
envelope above as read back for its whole span, finishes at or under
41:47.28, the fastest valid stage-3 sample.

**What M2 must do to delete toyos-ld.** It is frozen and links nothing the host
builds; what keeps it is that it is the one linker a ToyOS process can run,
shipped as `/system/bin/toyos-ld` by `system.toml`'s `[programs]` row and named
by the ToyOS-hosted rustc
(`issues/build/the-hosted-rustc-names-a-linker-toyos-does-not-have.md`). It
goes when lld runs in the guest: the row, the crate, its host tests and its
`src/sourcegate.rs` rows go together, the hosted rustc names `rust-lld`, and
the published crates.io crate is yanked.
