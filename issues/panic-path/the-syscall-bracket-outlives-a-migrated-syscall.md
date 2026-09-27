---
status: open
kind: defect
opened: 2026-09-27
---

# The syscall bracket outlives a migrated syscall

`percpu::in_syscall` is the one answer to "is this Ring 0 context on a
process's behalf": the panic handler recovers on it, and `blame` makes a Ring 0
user-address fault the process's on it. It compares this CPU's recorded
syscall task with the current task, and only `leave_syscall` clears the word,
on the CPU where the syscall ends (`kernel/src/arch/x86_64/syscall.rs`'s
`syscall_handler` is its one caller). `Hw::switch` in
`kernel/src/arch/x86_64/hw.rs` moves the current identity and never the
bracket.

So a syscall that parks on CPU A and finishes on CPU B leaves A's word naming
its thread. When that thread next runs in Ring 3 on A before any other syscall
enters there, an interrupt handler's panic or user-address fault on A reads
`in_syscall()` true: the thread is poisoned and the machine carries on, where
the kernel bug should have halted it.

**Evidence:** read from the code; no test stages it.

**Exit condition:** the bracket names a thread only while that thread is inside
a syscall on this CPU, and a guest test stages a migrated syscall followed by an
interrupt-context panic on the first CPU and sees the machine halt.
