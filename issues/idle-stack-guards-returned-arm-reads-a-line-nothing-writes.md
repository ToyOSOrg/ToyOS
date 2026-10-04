---
status: open
kind: tooling
opened: 2026-09-27
---

# `idle_stack_guard`'s "the read succeeded" arm reads a line nothing writes

`tests/common/faults.rs`'s `idle_stack_guard` reds with *the page below the idle
stack is still mapped* when its capture contains `debug syscall returned`. The
only guest it drives, `tests/toyos-rust-tests/src/bin/test_panic_child.rs`,
writes `SYS_DEBUG {action} returned {rc:#x}` when the syscall comes back. The
arm can never fire: a guard page that is still mapped reaches the drain's
ceiling and reds instead on the missing `#PF UNHANDLED` line, naming the wrong
cause.

**Evidence:** read from both sources; no mutation run.

**Exit condition:** the arm and the child share one constant for the line, and
a mutation that maps the guard page reds with the arm's own message.
