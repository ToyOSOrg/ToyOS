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
clang+lld against libc++.** It is a recipe, and a ToyOS run matches every
element of it:

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
- *C++ library*: libc++ with libc++abi from the same source, built in each
  stage's build by that stage's own clang (`runtimes`). The `s2` tools link
  it statically: `s2`'s `clang-22` and `lld` name no C++ shared object and
  reference no `GLIBCXX` symbol. `s2`'s libc++ headers are
  `_LIBCPP_VERSION` 220108.
- *Flags*, of `s2` and `s3` alike: `CMAKE_C_FLAGS` and `CMAKE_CXX_FLAGS`
  empty in the cache, their `_RELEASE` forms `-O3 -DNDEBUG`, and every
  `CMAKE_{EXE,SHARED,MODULE,STATIC}_LINKER_FLAGS`, `_RELEASE` included,
  empty; `CFLAGS`, `CXXFLAGS` and `LDFLAGS` unset. From `$LIBCXX`,
  configure adds `-stdlib=libc++` to the C++ compile and link lines and
  `-static-libstdc++` to the link lines
  (`llvm/cmake/modules/HandleLLVMStdlib.cmake`).
- *Build*: that clang+lld builds clang+lld from the same source with the
  `s3` lines below, configured afresh; cmake 3.28.3, ninja 1.11.1.
- *Timed span*: the last line's `ninja` alone.
- *Warm*: a span's configure line starts as soon as a complete `s3`
  `ninja` line exits, and its `ninja` as soon as that configure exits; the
  build reads no block from a block device during the span (`time -v`'s
  file system inputs 0).
- *Power envelope*, every element and its value:
  - AC online; `platform_profile` `performance`.
  - `MSR_PKG_POWER_LIMIT` (0x610) `0x0042820000dd8200`: PL1 64 W over
    28 s, PL2 64 W over 2.44 ms, both enabled, unlocked.
  - The package limit through MMIO, MCHBAR + 0x59A0,
    `0x0042820000dd80a0`: PL1 20 W over 28 s, PL2 64 W over 2.44 ms.
  - `MSR_VR_CURRENT_CONFIG` (0x601) `0x3c8`: the peak limit, 121 W.
  - On every CPU: `IA32_PM_ENABLE` (0x770) 1; `IA32_HWP_REQUEST` (0x774)
    `0x80002a04`, that is minimum 4, maximum 42, desired 0, EPP 128,
    activity window 0, package control off; `IA32_ENERGY_PERF_BIAS`
    (0x1B0) 6; `IA32_MISC_ENABLE` (0x1A0) bit 38 clear, turbo enabled.
  - `IA32_HWP_REQUEST_PKG` (0x772) `0x8000ff01`.

  Linux reaches these through intel_pstate `active` with `no_turbo` 0 and
  `hwp_dynamic_boost` 0 and, on every CPU, governor `powersave`, EPP
  `balance_performance`, `scaling_min_freq` 400000 and `scaling_max_freq`
  4200000; its read-backs include these.
- *Read-back*: every element of the power envelope and every CPU's
  microcode revision are read at the span's start, at its end, and between
  them at most 61 s apart from one read's start to the next, the sampler
  waiting 60 s after each read. A sample missing a read, or with any read
  off its value, is invalid.
- *Machine*: i5-1135G7, 8 threads, 16476082176 B RAM; BIOS `N34ET71W (1.71 )`;
  microcode revision (`IA32_BIOS_SIGN_ID`, 0x8B) `0xbe` on every CPU;
  Ubuntu 24.04.4, kernel 6.8.0-142-generic, command line
  `BOOT_IMAGE=/vmlinuz-6.8.0-142-generic root=/dev/mapper/ubuntu--vg-ubuntu--lv ro`,
  which leaves every mitigation at its default; ext4 on LVM.
- *Mitigations*: `/sys/devices/system/cpu/vulnerabilities` reads
  `gather_data_sampling` "Mitigation: Microcode",
  `indirect_target_selection` "Mitigation: Aligned branch/return thunks",
  `spec_store_bypass` "Mitigation: Speculative Store Bypass disabled via
  prctl", `spectre_v1` "Mitigation: usercopy/swapgs barriers and __user
  pointer sanitization" and `spectre_v2` "Mitigation: Enhanced / Automatic
  IBRS; IBPB: conditional; PBRSB-eIBRS: SW sequence; BHI: SW loop, KVM: SW
  loop", and every other entry "Not affected".

The lines, run in bash, which splits `$CONF` and `$LIBCXX`:

```
W=$HOME/llvm-baseline; SHA=52ed14fcd56afc30f9cccd8ca8ce237c2eef7e04
mkdir -p $W; cd $W
git init -q src && git -C src remote add origin https://github.com/rust-lang/llvm-project.git
git -C src fetch -q --depth 1 origin $SHA && git -C src checkout -q FETCH_HEAD
CONF="-G Ninja -DCMAKE_BUILD_TYPE=Release -DLLVM_ENABLE_PROJECTS=clang;lld -DLLVM_ENABLE_RUNTIMES=libcxx;libcxxabi;libunwind -DLIBCXX_STATICALLY_LINK_ABI_IN_STATIC_LIBRARY=ON -DLLVM_TARGETS_TO_BUILD=X86;AArch64 -DLLVM_ENABLE_ASSERTIONS=OFF -DLLVM_INCLUDE_TESTS=OFF -DLLVM_INCLUDE_BENCHMARKS=OFF -DLLVM_INCLUDE_EXAMPLES=OFF"
LIBCXX="-DLLVM_ENABLE_LIBCXX=ON -DLLVM_STATIC_LINK_CXX_STDLIB=ON"
rm -rf s1; cmake -S src/llvm -B s1 $CONF -DCMAKE_C_COMPILER=gcc -DCMAKE_CXX_COMPILER=g++ > s1-config.log
/usr/bin/time -v -o s1-time.txt ninja -C s1 -j8 clang lld runtimes > s1-build.log
rm -rf s2; PATH=$W/s1/bin:$PATH cmake -S src/llvm -B s2 $CONF $LIBCXX -DCMAKE_C_COMPILER=$W/s1/bin/clang -DCMAKE_CXX_COMPILER=$W/s1/bin/clang++ -DLLVM_ENABLE_LLD=ON > s2-config.log
PATH=$W/s1/bin:$PATH /usr/bin/time -v -o s2-time.txt ninja -C s2 -j8 clang lld runtimes > s2-build.log
rm -rf s3; PATH=$W/s2/bin:$PATH cmake -S src/llvm -B s3 $CONF $LIBCXX -DCMAKE_C_COMPILER=$W/s2/bin/clang -DCMAKE_CXX_COMPILER=$W/s2/bin/clang++ -DLLVM_ENABLE_LLD=ON > s3-config.log
PATH=$W/s2/bin:$PATH /usr/bin/time -v -o s3-time.txt ninja -C s3 -j8 clang lld > s3-build.log
```

The stage-3 spans measured: `s3-0`, the complete line the first span
follows, 42:37.88, not a sample; `s3-1` 42:36.03, valid; `s3-2` started,
its outcome unknown; `s3-3` not run. The bar is the best of three valid
samples, and it is not yet set.

Owed once the T14 answers again
(`issues/hardware/a-t14-measurement-has-no-way-back-but-its-wifi.md`):

- the `s3-2` and `s3-3` spans, and one more span for each that is invalid;
- `ninja -t commands clang lld` of `s2` and of `s3`, compared: they may
  differ only in the build directory;
- the compile line `s3` records for `clang/tools/driver/driver.cpp` under
  libc++, into *Flags*;
- the end-of-run machine read;
- the driver (pid 224932) and the root sampler (pid 224450) stopped, if
  either still runs.

*Exit*: a ToyOS build of the source above with the `s3` configuration and
flags, by a clang+lld built by the recipe above, on this T14, warm and
valid, with the *Read-back* at the Linux read-back's cadence, whose `ninja`
exits 0 at or under the bar.

**What M2 must do to delete toyos-ld.** It is frozen and links nothing the host
builds; what keeps it is that it is the one linker a ToyOS process can run,
shipped as `/system/bin/toyos-ld` by `system.toml`'s `[programs]` row and named
by the ToyOS-hosted rustc
(`issues/build/the-hosted-rustc-names-a-linker-toyos-does-not-have.md`). It
goes when lld runs in the guest: the row, the crate, its host tests and its
`src/sourcegate.rs` rows go together, the hosted rustc names `rust-lld`, and
the published crates.io crate is yanked.
