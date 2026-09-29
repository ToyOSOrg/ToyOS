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
locale support or libc++'s no-localization build, and
`dl_iterate_phdr` in libc (`issues/build/libc-has-no-dl-iterate-phdr.md`). M4 needs git in the guest, storage durable and fast
enough for an LLVM build tree
(`issues/filesystem/storage-is-layers-and-a-role-is-a-filesystem.md`), and
memory beyond what 2 MiB process pages allow
(`issues/kernel/process-memory-is-2-mib-pages-and-that-caps-the-process-count.md`).

**The bar: Linux on this T14 building the same LLVM with a clang-built
clang+lld against libc++.** It is a recipe, and a ToyOS run matches every
element of it. `toyos-llvmbar` holds its lines (`t14/block.txt`), the steps
the T14 runs (`t14/`), the power envelope and the judge, whose module header
is the rule a sample is valid by.

- *Source*: `rust-lang/llvm-project` at
  `52ed14fcd56afc30f9cccd8ca8ce237c2eef7e04`. A ToyOS run's source, for
  the building compiler and the build alike, differs from it by nothing
  but the fork's commits over that base (`forks.toml`, `[llvm-project]`).
- *Building compiler*: clang+lld at `52ed14fc`, Release,
  `LLVM_ENABLE_LTO=OFF`, `LLVM_BUILD_INSTRUMENTED=OFF`, no profile data,
  built by clang+lld at `52ed14fc` with the same configuration and flags:
  the `s2` lines. Stage 1 is only how the first clang exists: gcc, which
  has no ToyOS target, builds it with the `s1` lines
  (`gcc (Ubuntu 13.3.0-6ubuntu2~24.04.1) 13.3.0`). For ToyOS it is M2's
  cross build of the same source and configuration, whose lines M2 owes.
- *C++ library*: libc++ with libc++abi from the same source, built in each
  stage's build by that stage's own clang (`runtimes`). The `s2` tools link
  it statically: `s2`'s `clang-22` and `lld` name no C++ shared object and
  reference no `GLIBCXX` symbol. `s2`'s libc++ headers are
  `_LIBCPP_VERSION` 220108.
- *Build*: that clang+lld builds clang+lld from the same source with the
  `s3` lines, configured afresh; cmake 3.28.3, ninja 1.11.1.
- *Timed span*: the last line's `ninja` alone.
- *Warm*: a span's configure line starts as soon as a complete `s3`
  `ninja` line exits, and its `ninja` as soon as that configure exits; the
  build reads no block from a block device during the span (`time -v`'s
  file system inputs 0).
- *Power envelope*: `toyos-llvmbar`'s `ENVELOPE` and `ENVELOPE_EVERY_CPU`,
  every element with its value; `LINUX` and `LINUX_EVERY_CPU` are how Linux
  reaches them.
- *Machine*: i5-1135G7, 8 threads, 16476082176 B RAM; microcode revision
  `0xbe` on every CPU; Ubuntu 24.04.4 with the kernel
  `issues/kernel/the-kernel-mitigates-what-linux-mitigates-on-the-t14.md`
  pins, 6.8.0-142-generic; ext4 on LVM.
- *Mitigations*: that track's S0 capture is the one read of them. A sample
  reads the kernel, the vulnerabilities and the microcode at its start and
  its end with S0's own commands, and the judge refuses it unless they are
  S0's.

The bar is the best of three valid samples, and it is not set: no sample is
valid by the judge.

Owed once the T14 answers again
(`issues/hardware/a-t14-measurement-has-no-way-back-when-the-t14-stops-answering.md`),
and Ubuntu is wiped only after S0's fixtures and the bar are both committed:

- S0's capture, whose text is the judge's second argument;
- `t14/driver.sh` under `t14/sampler.sh`, run until `toyos-llvmbar` exits
  0, and the bar, the BIOS version and the kernel command line it prints
  recorded here;
- *Flags*, of `s2` and `s3`: the `CMAKE_*_FLAGS` and linker flags the
  run's `*-cache.txt` hold, and the compile line `s3-commands.txt` holds
  for `clang/tools/driver/driver.cpp` and the link line for `clang`, which
  show whether `$LIBCXX` added `-stdlib=libc++` and `-static-libstdc++`;
- `s2-commands.txt` and `s3-commands.txt` compared: they may differ only in
  the build directory.

*Exit*: a ToyOS build of the source above with the `s3` configuration and
flags, by a clang+lld built by the recipe above, on this T14, warm, whose
`ninja` exits 0 at or under the bar, read back as the judge reads a Linux
sample: the envelope at its cadence from a start read to an end read, and at
the span's start and end the microcode, the BIOS version (SMBIOS type 0)
equal to the Linux sample's, and for each entry S0 captures as a mitigation
that track's *Exit* line with its mechanism in force on every CPU:

- `gather_data_sampling`: `IA32_MCU_OPT_CTRL` (0x123) `GDS_MITG_DIS` clear
  (S2);
- `spectre_v2`: `IA32_SPEC_CTRL` (0x48) at S0's Linux read with SSBD masked
  (S2);
- `indirect_target_selection`: the live thunk bodies equal to the body S1
  selects (S5);
- `spectre_v1`: CR4.SMAP set (S4);
- `spec_store_bypass`: where S0 captures Linux's prctl mode, SSBD is set only
  in a process that asked by prctl, so parity is SSBD set in exactly the
  build's processes that ask Linux, read on the switch into each (S6); which
  of them ask is owed.

An entry S0 captures as a mitigation that this list does not name holds the
exit until it is named here.

**What M2 must do to delete toyos-ld.** It is frozen and links nothing the host
builds; what keeps it is that it is the one linker a ToyOS process can run,
shipped as `/system/bin/toyos-ld` by `system.toml`'s `[programs]` row and named
by the ToyOS-hosted rustc
(`issues/build/the-hosted-rustc-names-a-linker-toyos-does-not-have.md`). It
goes when lld runs in the guest: the row, the crate, its host tests and its
`src/sourcegate.rs` rows go together, the hosted rustc names `rust-lld`, and
the published crates.io crate is yanked.
