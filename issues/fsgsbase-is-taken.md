---
status: open
kind: track
opened: 2026-09-29
---

# FSGSBASE is taken

`CR4.FSGSBASE` is forbidden (`kernel/src/arch/x86_64/control_regs.rs:62-75`):
GS always holds the kernel's per-CPU base and the kernel runs no `swapgs`.
Taking it costs a `swapgs` entry and the swapgs barriers of
`issues/user-pointer-checks-have-no-spectre-v1-fence-and-smap-is-optional.md`.
Intel names what it gives, user programs' FS and GS bases without a system
call and a more efficient kernel context switch, and has it used in production
only with a complete OS implementation (Guidance for Enabling FSGSBASE, ID
767615).

**Exit**: on every proving machine and under TCG, a `boot-actuators` arm
enters vector 2 at CPL 0 while the user GS base is live, between entry and its
`swapgs` or after the exit path's, and the handler finds the kernel's per-CPU
data; `CR4_FORBIDDEN`, the `user-writable-gsbase` kernel
(`build/build.rs:1370`) and the test `gsbase_locked` are deleted; the T14's
pipe round trips read with and without it. **Mutation**: an IST entry that
decides `swapgs` from the saved CS alone. **Oracle**: Linux's
`paranoid_entry`.
