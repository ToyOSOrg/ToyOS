---
status: open
kind: defect
opened: 2026-09-29
---

# The kernel stack canary is one global, where Linux's is per task

S9 of `issues/kernel/the-kernel-mitigates-what-linux-mitigates-on-the-t14.md`
seeds one `__stack_chk_guard` for the whole kernel, because LLVM reads that
global on `x86_64-unknown-none`. Linux on the T14 draws a canary per task
(`kernel/fork.c:1161` at `Ubuntu-6.8.0-142.142`) and
loads the next task's on every switch (`arch/x86/entry/entry_64.S:195`), so a
canary leaked from one thread's stack does not forge another's. Here it
forges every thread's. The gap opens when S9 lands and is owned by that
track.

**Exit**: the canary a protected kernel frame checks is the running thread's
own, and a test that reads two threads' canaries finds them different.
