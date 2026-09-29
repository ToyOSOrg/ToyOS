---
status: open
kind: track
opened: 2026-09-29
---

# Kernel forward copies and fills are one `rep movsb` or `rep stosb` on every CPU

The kernel's `memcpy`, `memset` and `memmove` are `compiler_builtins`', which
frame `rep movsq` or `rep stosq` with two byte strings, and take one `rep movsb`
or `rep stosb` only when built with the `ermsb` target feature
(`rust/library/compiler-builtins/compiler-builtins/src/mem/x86_64.rs:15-17,23-58,88-126`);
`memmove`'s backward branch, `copy_backward` (`:62-85`), keeps `rep movsq`
either way. The kernel links the sysroot's `compiler_builtins`, which no kernel
build flag recompiles: the feature is `rustflags = ["-Ctarget-feature=+ermsb"]`
under `[target.x86_64-unknown-none]` in the std build's `bootstrap.toml`
(`std_config`, `src/sysroot.rs:522`; bootstrap's `core/config/toml/target.rs:41`),
and it moves the sysroot key (`RECIPE`, `src/sysroot.rs:65`), which reaches
CI's installed toolchain only once the release tag's `trees()`
(`src/release.rs:25-31`) hashes `src/sysroot.rs`, the work of
`issues/build/the-release-tag-hashes-none-of-the-build-system-that-builds-the-toolchain.md`,
which lands first. One path on every
CPU: `rep movsb` is correct without ERMS, CPUID.(7,0):EBX bit 9, and no CPU is
refused. Zen 2 lacks ERMS (a Ryzen 9 PRO 3900, family 0x17, reads EBX
0x219C91A9: InstLatx64 ddff8a92,
`AuthenticAMD/AuthenticAMD0870F10_K17_Matisse_CPUID.txt:52`), and no proving
machine is family 0x17.

**Exit**: a gate over `kernel.elf`, on the decoder of
`issues/build/no-gate-decodes-kernel-elfs-instructions.md`, finds `memcpy`,
`memset` and `memmove`'s forward branch each one `rep movsb` or `rep stosb`,
with no `rep movsq` or `rep stosq`; the T14's pipe figures; and Zen 2's cost
recorded, as a family-0x17 figure against the build without `ermsb` or as a
`tooling` issue that owes it. **Mutation**: the build without `ermsb` reds the
gate. **Oracle**: Linux's pipe figures.
