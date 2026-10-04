---
status: open
kind: defect
opened: 2026-09-29
---

# User programs run without a shadow stack

At `Ubuntu-6.8.0-142.142`, `CONFIG_X86_USER_SHADOW_STACK=y` (config line 502)
lets a process opt into CET shadow stacks: a return address a `RET` pops is
checked against a hardware-maintained copy that ordinary writes cannot reach,
so a corrupted return address on the data stack is refused rather than taken.
ToyOS builds no user shadow stack: no program can enable one, and nothing in
`kernel/src` maps a shadow-stack page or sets `CR4.CET`/`MSR_IA32_U_CET`.
ToyOS does not make it opt-in: the loader refuses every program and library
without `GNU_PROPERTY_X86_FEATURE_1_SHSTK`, on every CPU, with or without
shadow-stack hardware.

**Exit**: the guest test `unmarked_object_refused` passes on every proving
machine and under TCG; a sysroot link with one unmarked input reds, since lld
ANDs the property; on the T14, the one proving machine known to enumerate
shadow stacks, a forged return is `#CP` in two threads, a caught panic
unwinds, a store to the shadow stack is `#PF`, and `user_ptr` refuses a `read`
into it with `BadAddress`. **Mutation**: the property check gone; `SH_STK_EN`
clear; one shadow stack for two threads; no `incssp`; the stack mapped
writable or faulted in as CoW. **Oracle**: the T14's CPU.
