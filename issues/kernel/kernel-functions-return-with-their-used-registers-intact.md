---
status: open
kind: defect
opened: 2026-09-29
---

# Kernel functions return with their used registers intact

The kernel is built with no option that zeroes call-used registers on return
(`kernel/.cargo/config.toml`), so every function leaves its scratch values in
the registers its caller and the next gadget see. Linux at
`Ubuntu-6.8.0-142.142`, under the T14's `CONFIG_ZERO_CALL_USED_REGS=y`,
builds with `-fzero-call-used-regs=used-gpr` (`Makefile:891-894`). LLVM acts
on the function attribute `zero-call-used-regs`
(`llvm/lib/CodeGen/PrologEpilogInserter.cpp:1212`), and the rustc fork at
1b236638 has no option that sets it
(`compiler/rustc_session/src/options.rs`). A row of the hardening table in
`issues/kernel/the-kernel-mitigates-what-linux-mitigates-on-the-t14.md`.

**Exit**: the kernel, `core` and `alloc` build under a rustc option that sets
`zero-call-used-regs=used-gpr` on every function, carried in the fork to
upstream quality; a gate over `kernel.elf` finds each function's used
call-clobbered general registers zeroed before its return thunk, and a build
without the option reds it.
