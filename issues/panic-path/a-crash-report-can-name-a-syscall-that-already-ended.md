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
bracket. The number, the user `rip` and the `rbp` the user backtrace walks from
are per-CPU copies too (`syscall_entry` in `kernel/src/arch/x86_64/syscall.rs`).

A syscall that parks on CPU A and finishes on CPU B is wrong both ways:

- **A keeps naming it.** A's word still names the thread. When that thread next
  runs in Ring 3 on A before any other syscall enters there, an interrupt
  handler's death on A reports the finished syscall's number, user `rip` and
  user backtrace as the context it died in.
- **B forgets another.** `leave_syscall` on B clears whatever B's word named,
  even a second thread still parked inside a syscall it entered on B. When that
  thread resumes on B and the kernel dies inside its syscall, the report prints
  no `Syscall:` line.

`process::handle_fault` (`kernel/src/process.rs`) prints `percpu::syscall_num()`
with no bracket at all, so a handle fault in a syscall that migrated names the
syscall that last entered on the CPU it faulted on.

`syscall_entry` already pushes the user `rsp`, the user `rip` and the number at
the top of the thread's own kernel stack, which moves with the thread; reading
them there would delete every per-CPU copy. The frame alone cannot say whether
it is a syscall's: an interrupt from Ring 3 puts `SS` and `CS` in the slots
where a syscall's frame holds the user's `rsp` and `rdi`, and userland chooses
both.

**Evidence:** read from the code; no test stages it.

**Exit condition:** the syscall context a crash report or a handle fault prints
is the current thread's, whichever CPU it entered on, and a guest test stages
both migrations: an interrupt-context death on the first CPU whose report
carries no `Syscall:` line, and a death inside a syscall that parked while
another thread's syscall ended on its CPU, whose report carries its own.
