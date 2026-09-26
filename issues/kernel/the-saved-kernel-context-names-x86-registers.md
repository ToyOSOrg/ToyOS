---
status: open
kind: defect
opened: 2026-09-26
---

# The saved kernel context names x86 registers in generic code

`kernel/src/sched/payload.rs`'s `KernelCtx`, which every CPU's switch loads,
carries `rsp` and `fs_base`: the stack pointer and the user thread pointer by
their x86 names, in code outside `arch/`. AArch64 saves `sp` and
`TPIDR_EL0` in the same two roles, so the port either writes AArch64 state
into fields named for x86 or grows a second copy of the struct.

Owned by stage 4 of `issues/kernel/toyos-runs-on-arm64.md`, which writes the
AArch64 context switch.

**Exit condition**: the fields are named by role (the saved kernel stack
pointer, the user thread pointer), the x86 and AArch64 switches both load
them, and nothing outside `kernel/src/arch/` names `rsp` or `fs_base`.
