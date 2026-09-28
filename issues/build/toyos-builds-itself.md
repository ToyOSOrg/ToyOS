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
  `52ed14fcd56afc30f9cccd8ca8ce237c2eef7e04`. A ToyOS run's source, for
  the building compiler and the build alike, differs from it by nothing
  but the fork's commits over that base (`forks.toml`, `[llvm-project]`).
- *Building compiler*: clang+lld at `52ed14fc`, Release,
  `LLVM_ENABLE_LTO=OFF`, `LLVM_BUILD_INSTRUMENTED=OFF`, no profile data,
  built by clang+lld at `52ed14fc` with the same configuration and flags:
  the `s2` lines below. For ToyOS it is cross-built with those lines for
  the host triple `x86_64-unknown-toyos`. Stage 1 is only how the first
  clang exists: gcc, which has no ToyOS target, builds it with the `s1`
  lines (`gcc (Ubuntu 13.3.0-6ubuntu2~24.04.1) 13.3.0`).
- *Flags*, of `s2` and `s3` alike: `CMAKE_C_FLAGS` and `CMAKE_CXX_FLAGS`
  empty, their `_RELEASE` forms `-O3 -DNDEBUG`, and every
  `CMAKE_{EXE,SHARED,MODULE,STATIC}_LINKER_FLAGS`, `_RELEASE` included,
  empty; `CFLAGS`, `CXXFLAGS` and `LDFLAGS` unset. The two builds'
  `ninja -t commands clang` differ only in the build directory. The
  compile line `s3` records for `clang/tools/driver/driver.cpp`, with `$W`
  for the work directory:

  ```
  $W/s2/bin/clang++ -DLLVM_BUILD_STATIC -D_GLIBCXX_USE_CXX11_ABI=1 -D_GNU_SOURCE -D__STDC_CONSTANT_MACROS -D__STDC_FORMAT_MACROS -D__STDC_LIMIT_MACROS -I$W/s3/tools/clang/tools/driver -I$W/src/clang/tools/driver -I$W/src/clang/include -I$W/s3/tools/clang/include -I$W/s3/include -I$W/src/llvm/include -fPIC -fno-semantic-interposition -fvisibility-inlines-hidden -Werror=date-time -Werror=unguarded-availability-new -Wall -Wextra -Wno-unused-parameter -Wwrite-strings -Wcast-qual -Wmissing-field-initializers -pedantic -Wno-long-long -Wc++98-compat-extra-semi -Wimplicit-fallthrough -Wcovered-switch-default -Wno-noexcept-type -Wnon-virtual-dtor -Wdelete-non-virtual-dtor -Wsuggest-override -Wstring-conversion -Wno-pass-failed -Wmisleading-indentation -Wctad-maybe-unsupported -fdiagnostics-color -ffunction-sections -fdata-sections -fno-common -Woverloaded-virtual -Wno-nested-anon-types -O3 -DNDEBUG -std=c++17 -fno-exceptions -funwind-tables -fno-rtti -MD -MT tools/clang/tools/driver/CMakeFiles/clang.dir/driver.cpp.o -MF tools/clang/tools/driver/CMakeFiles/clang.dir/driver.cpp.o.d -o tools/clang/tools/driver/CMakeFiles/clang.dir/driver.cpp.o -c $W/src/clang/tools/driver/driver.cpp
  ```
- *Build*: that clang+lld builds clang+lld from the same source with the
  `s3` lines below, configured afresh; cmake 3.28.3, ninja 1.11.1.
- *Timed span*: the last line's `ninja` alone.
- *Warm*: each span starts within 8 s of the end of a complete `s2` or
  `s3` `ninja` line, the package at 68 to 75 °C, and the build reads no
  block from a block device during the span.
- *Power envelope*: on AC; `platform_profile` `performance`. RAPL through
  the MSR: PL1 64 W, PL2 64 W, peak 121 W. RAPL through MMIO: PL1 20 W,
  PL2 64 W, peak 121 W. Both PL1 windows 27983872 µs. On every CPU:
  `IA32_PM_ENABLE` (0x770) 1; `IA32_HWP_REQUEST` (0x774) `0x80002a04`,
  that is minimum 4, maximum 42, desired 0, EPP 128, activity window 0,
  package control off; `IA32_ENERGY_PERF_BIAS` (0x1B0) 6;
  `IA32_MISC_ENABLE` (0x1A0) bit 38 clear, turbo enabled.
  `IA32_HWP_REQUEST_PKG` (0x772) `0x8000ff01`. Linux reaches the HWP
  values through intel_pstate active, governor `powersave`, EPP
  `balance_performance`. AC, `platform_profile`, the RAPL limits, the
  governor and EPP are read back every 60 s and at the span's start and
  end. The PL1 windows and the MSRs of HWP, EPB and turbo are read once,
  not per span: those after the spans, in the same boot, with governor,
  EPP and `platform_profile` unchanged in every read-back of every span.
- *Validity*: a sample counts only if every read-back during its span
  matches the power envelope, and the package power over the span's first
  60 s, from the energy counter (`MSR_PKG_ENERGY_STATUS`, 0x611), is at
  most 21.2 W.
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

The valid stage-3 samples: 41:47.28, 41:48.02 and 41:49.40.

*Exit*: a ToyOS build of the source above with the `s3` configuration and
flags, by a clang+lld built by the recipe above, on this T14, warm and
valid, reading back for its whole span PL1 and PL2, every CPU's HWP
request, the package request, EPB and the turbo bit at the envelope's
values, whose `ninja` exits 0 at or under 41:47.28, the fastest valid
stage-3 sample.

**What M2 must do to delete toyos-ld.** It is frozen and links nothing the host
builds; what keeps it is that it is the one linker a ToyOS process can run,
shipped as `/system/bin/toyos-ld` by `system.toml`'s `[programs]` row and named
by the ToyOS-hosted rustc
(`issues/build/the-hosted-rustc-names-a-linker-toyos-does-not-have.md`). It
goes when lld runs in the guest: the row, the crate, its host tests and its
`src/sourcegate.rs` rows go together, the hosted rustc names `rust-lld`, and
the published crates.io crate is yanked.
