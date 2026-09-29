---
status: open
kind: defect
opened: 2026-09-29
---

# An AArch64 crash report reads through any user leaf

`kernel/src/arch/aarch64/paging.rs`'s `read_user_word`, which the report
of an EL0 fault calls with the faulting thread's own `x29` to walk its
frames (`trap.rs`'s `user_backtrace`), reads the frame any valid user leaf
names through the direct map. The direct map holds memory and nothing
else, so a leaf naming a device's registers — a claimed function's BAR,
once the port's stage 6 maps one into a process — names an address the
direct map does not hold, and the report's read of it is an EL1 data abort:
a user fault with `x29` pointed into its own BAR ends the machine.

Nothing maps a BAR into an AArch64 process yet, so this is latent until
stage 6 of `issues/kernel/toyos-runs-on-arm64.md`.

**Exit condition**: `read_user_word` reads only a leaf of the memory type
the direct map holds, and a guest test whose process faults with `x29`
inside a mapped BAR sees its process end and the kernel live.
