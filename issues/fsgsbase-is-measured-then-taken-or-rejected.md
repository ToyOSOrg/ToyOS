---
status: open
kind: track
opened: 2026-09-29
---

# FSGSBASE is measured, then taken or rejected

`CR4.FSGSBASE` is forbidden (`kernel/src/arch/x86_64/control_regs.rs:62-75`):
GS always holds the kernel's per-CPU base and the kernel runs no `swapgs`.
Taking it costs a `swapgs` entry and the swapgs barriers of
`issues/user-pointer-checks-have-no-spectre-v1-fence-and-smap-is-optional.md`.

**Exit**: the T14's pipe round trips with and without it, a loss becoming a
`rejected` issue with both figures. Taken: on every proving machine and under
TCG, a `boot-actuators` arm enters vector 2 at CPL 0 while the user GS base is
live, between entry and its `swapgs` or after the exit path's, and the handler
finds the kernel's per-CPU data; `CR4_FORBIDDEN`, the `user-writable-gsbase`
kernel (`src/build.rs:1370`) and the test `gsbase_locked` are deleted.
**Mutation**: an IST entry that decides `swapgs` from the saved CS alone.
**Oracle**: Linux's `paranoid_entry`.
