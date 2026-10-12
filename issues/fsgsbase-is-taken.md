---
status: open
kind: track
opened: 2026-09-29
---

# FSGSBASE is taken

`CR4.FSGSBASE` is forbidden (`kernel/src/arch/x86_64/control_regs.rs:149-153`):
GS always holds the kernel's per-CPU base and the kernel runs no `swapgs`.
Taking it costs a `swapgs` entry and the swapgs barriers of
`issues/user-pointer-checks-have-no-spectre-v1-fence-and-smap-is-optional.md`.
Intel names what it gives, user programs' FS and GS bases without a system
call and a more efficient kernel context switch, and has it used in production
only with a complete OS implementation: one whose context switch manages the
FS and GS bases apart from their selectors and whose ring transitions are
insensitive to the GS base, as Linux's is from 5.9 on (Guidance for Enabling
FSGSBASE, ID 767615). Linux enables it on every CPU that enumerates it
(`arch/x86/kernel/cpu/common.c:1515-1519` at `v5.9`); its switch saves each
thread's bases as the thread left them (`process_64.c:248-262`), and its
paranoid entry assumes nothing of the GS base it finds and loads the CPU's own
from a kernel table (`entry_64.S:823-848`). AMD documents the four
instructions, enabled by CR4.FSGSBASE at every privilege level, and recommends
nothing on their use (APM 24594 rev. 3.36, Vol. 3, RDFSBASE and WRFSBASE).
ToyOS takes it under Intel's condition: its switch saves and restores each
thread's FS and GS bases as the thread left them, and its ring transitions are
insensitive to the GS base they find. Its figure is reported and decides
nothing.

**Exit**: on every proving machine and under TCG, a `boot-actuators` arm
enters vector 2 at CPL 0 while the user GS base is live, between entry and its
`swapgs` or after the exit path's, and the handler finds the kernel's per-CPU
data; a thread's FS and GS bases, written with WRFSBASE and WRGSBASE, read
back unchanged after its CPU has run another thread; `CR4_FORBIDDEN` is
deleted, and so is `gsbase_locked` with its `user-writable-gsbase` kernel from
stage C of `issues/the-guest-suite-runs-only-what-no-cheaper-tier-reaches.md`;
the T14's pipe round trips read with and without FSGSBASE. **Mutation**: an
IST entry that decides `swapgs` from the saved CS alone. **Oracle**: Linux's
`paranoid_entry`.
