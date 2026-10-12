---
status: open
kind: tooling
opened: 2026-10-12
---

# Nothing reads the AArch64 kernel's instructions for FP/SIMD use

A thread's FP/SIMD state stays in the registers across every entry from EL0
and is saved only at a switch (`kernel/src/arch/aarch64/fpu.rs`). A kernel
instruction outside `arch::aarch64::switch` that writes a `v` register, `FPCR`
or `FPSR` therefore corrupts a thread with no fault.

The only check is `assert_aarch64_kernel_is_softfloat` in `build/build.rs`. It
reads the target's `cfg` (`-fp-armv8,-neon`), which refuses FP/SIMD the compiler
generates but nothing written as `asm!` text, and `context_switch` turns on
`.arch_extension fp` and `simd` for its own block. The x86-64 kernel is judged
by its decoded instructions (`build/fpgate.rs`). The AArch64 kernel is not.

**Exit**: the build decodes the AArch64 kernel image and refuses it when an
instruction outside `arch::aarch64::switch` reads or writes FP/SIMD state. The
gate has a host test for each refused class, and a build with one such
instruction planted goes red.
