---
status: open
kind: track
opened: 2026-09-01
---

# Exactly one allocation can be made to fail, at exactly one site, and there is no `alloc_error_handler`

`git grep alloc_error_handler -- kernel/` returns nothing: the kernel has no
allocation-error handler at all, which is what
`issues/kernel/no-alloc-error-handler.md` records. That half is simply true.

**What exists, and why it is not enough.** `debug_action::HEAP_AT_CEILING_PAGE_ALIGNED`
(a `pub const` in `toyos-abi/src/syscall.rs`'s `debug_action` module, `:709` and `:723`;
dispatched at `kernel/src/arch/syscall/dispatch.rs:538`) drives `debug_heap_alloc` at
`MAX_HEAP_ALLOC` with 4096-byte alignment, which the page source cannot back;
`kernel/src/arch/syscall/debug.rs:25-27` reports the null as
`ResourceExhausted` rather than unwrapping it, and
`tests/toyos-rust-tests/src/bin/heap_ceiling.rs:131` asserts exactly that. So a
test-only actuator does exist, a shipped test does read it, and the claim that
nothing here can be made to fail on purpose is false. `early_alloc` has a second
null return of its own (`kernel/src/mm/alloc.rs:568-569`) before the allocator
is initialised.

What that actuator cannot do is fail an **arbitrary** allocation at an
**arbitrary** site. It is one fixed size at one fixed entry point, chosen
because it is the size the page source refuses. Nothing can make the third
allocation inside a driver's probe path fail, which is where the terminal path
actually needs testing — and an ordinary stress test is no substitute, because
exhausting the heap changes everything else the kernel is doing at the same
time, and the report you need is written by a path that must not allocate.

**What to build.** A test-only countdown actuator: arm it with N, and the Nth
allocation after arming fails, wherever it falls. Enter it immediately before
the code under test. Capture the result over serial and the panel — channels
that do not allocate — under a host deadline, so "nothing was reported" is a
verdict rather than a hang.

**Two properties it owes.** Its own accounting must not allocate, and it must
not perturb allocator order — proved by showing that with the countdown
disabled the allocator's behaviour is byte-for-byte what it was. The result
channel is preallocated at arm time, before the failure it exists to report.

`issues/kernel/nothing-charges-kernel-memory-to-a-process.md` ends by naming
`no-alloc-error-handler` as the terminal state every unbounded grower closes
into. Whichever of the two lands first, the other's closure runs through this
actuator; they are not two answers to one question.
