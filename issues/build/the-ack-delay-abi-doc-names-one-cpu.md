---
status: open
kind: tooling
opened: 2026-09-14
---

# `TLB_ACK_DELAY_ARM`'s ABI doc still names one CPU

`toyos-abi/src/syscall.rs`'s `debug_action::TLB_ACK_DELAY_ARM` reads

> Make the last CPU a shootdown waits for answer `arg` nanoseconds late, and
> take it away again.

The kernel arm no longer picks a CPU. `kernel/src/arch/tlb.rs`'s
`stage_ack_delay` holds the acknowledgement back on every path a CPU answers
*another* CPU's shootdown on, so the arming reaches every CPU an initiator
waits for and none that it does not. The "last CPU" the doc names was chosen by
a `last_target()` that could name the initiator itself — which is what made
`tlb_shootdown_waits` measure nothing and red on CI — and that function is
gone.

So the one sentence userland reads to learn what action 12 does describes a
selection the kernel no longer makes.

**Why it was not fixed alongside the change that made it stale.**
`toyos-abi/src` is one of `toolchain::SYSROOT_SOURCES`, and
`pr::abi_lands_alone` refuses a branch that mixes a commit touching those paths
with commits that do not, regardless of order; a doc comment is a touch like
any other. The correction is one sentence and belongs on its own single-commit
branch, which is the whole of this entry.

**Exit condition.** The doc names what the kernel does: every acknowledgement
an initiator waits for, not one CPU chosen by arithmetic.
