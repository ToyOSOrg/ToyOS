---
status: open
kind: defect
opened: 2026-09-29
---

# The kernel runs without a shadow stack

Nothing checks a kernel return, so an overwritten kernel return address is
taken. Linux at `Ubuntu-6.8.0-142.142` has no kernel shadow stack; the kernel
takes one where the CPU enumerates supervisor shadow stacks, CET_SSS,
CPUID.(7,1):EDX bit 18, which the T14 reads set (EDX 0x00040000).

**Exit**: on the T14, the one proving machine known to enumerate CET_SSS, a
forged kernel return is `#CP`, the guest test `kernel_stack_canary` of
`issues/the-kernel-has-no-stack-protector.md` runs under it, and a
store through `DirectMap` to a shadow stack faults. **Mutation**: `SH_STK_EN`
clear; SSP not switched; the frame left in the writable direct map.
**Oracle**: the T14's CPU.
