---
status: open
kind: defect
opened: 2026-09-28
---

# A new x86-64 thread enters Ring 3 holding the kernel's register values

`kernel/src/arch/x86_64/entry.rs`'s `process_start` and `thread_start` call
`sched::driver::trampoline_entry`, restore only `r12`–`r14` and the FP state,
and `iretq`: every other general register — `rax`–`rdx`, `rsi`, `rbp`,
`r8`–`r11`, `r15`, and `rdi` in a process's case — reaches the thread's first
instruction holding whatever the kernel left there, kernel stack and heap
addresses among them. A thread reads the kernel's layout off its own
registers.

The AArch64 trampoline zeroes every register but the argument before its
`ERET`, and is the shape to copy.

**Exit condition**: a fresh thread's first instruction sees zero in every
general register but its stack pointer and its argument, and a guest test that
reads them at `_start` says so.
