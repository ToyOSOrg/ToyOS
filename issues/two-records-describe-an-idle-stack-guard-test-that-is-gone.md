---
status: open
kind: tooling
opened: 2026-11-11
---

# Two records describe an `idle_stack_guard` test that is gone

`issues/idle-stack-guard-price-nearly-doubled-since-its-return.md` and
`issues/idle-stack-guards-returned-arm-reads-a-line-nothing-writes.md` are
both about `idle_stack_guard` in `tests/common/faults.rs`. No function, row
or registration of that name is in the tree (`git grep idle_stack_guard`
answers only these two files), so neither record's exit can be met as
written. `SYS_DEBUG`'s `IDLE_GUARD_READ` (`kernel/src/syscall/dispatch.rs`,
`sdk/abi/src/syscall.rs`), the action that test drove, has no caller left
either.

`a_kernel_stack_overflow_names_its_stack` now holds what the test held: a
CPU's idle stack run off its bottom faults on its guard.

**Exit**: both records are closed with the commit that cut the test named, and
`IDLE_GUARD_READ` and `percpu::idle_guard_byte` go with them.
