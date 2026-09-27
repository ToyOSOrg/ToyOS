---
status: open
kind: defect
opened: 2026-09-27
---

# A crash report can name a syscall that already ended

The crash report prints `Syscall:` and a user backtrace only while
`percpu::in_syscall()` holds (`kernel/src/arch/x86_64/idt/exceptions.rs`). That
compares this CPU's recorded syscall task with the current task, and only
`leave_syscall` clears the word, on the CPU where the syscall ends. `Hw::switch`
in `kernel/src/arch/x86_64/hw.rs` moves the current identity and never the
bracket.

So a syscall that parks on CPU A and finishes on CPU B leaves A's word naming
its thread. When that thread next runs in Ring 3 on A before any other syscall
enters there, an interrupt handler's death on A reports the finished syscall's
number, user `rip` and user backtrace as the context it died in.

**Evidence:** read from the code; no test stages it.

**Exit condition:** the bracket names a thread only while that thread is inside
a syscall on this CPU, and a guest test stages a migrated syscall followed by an
interrupt-context death on the first CPU whose report carries no `Syscall:`
line.
