---
status: open
kind: defect
opened: 2026-09-29
---

# The kernel has no stack protector

Nothing in the build asks rustc for a stack protector, so an overflow of a
kernel stack buffer reaches the return address unchecked. Linux at
`Ubuntu-6.8.0-142.142` builds with `STACKPROTECTOR_STRONG`
(`debian.master/config/annotations:13402`), its guard at a fixed `%gs` offset
and switched per task.

**Exit**: the kernel builds with a strong stack protector whose guard is at
`%gs:<offset>`, through LLVM and rustc changes the orchestrator admitted as
upstream-quality cross-platform options; `stack-protector-3.ll` and a rustc
assembly test see `%gs:<offset>` and no `__stack_chk_guard`; on every proving
machine the guest test `kernel_stack_canary` panics on thread B's guard in A's
frame. **Mutation**: the old lowering; the flags dropped; `context_switch`'s
write gone; a constant guard. **Oracle**: FileCheck, and the CPU's compare.
