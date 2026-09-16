---
status: open
kind: defect
opened: 2026-09-14
---

# `TLB_ACK_DELAY_ARM`'s ABI doc still names one CPU

`toyos-abi/src/syscall.rs`'s `debug_action::TLB_ACK_DELAY_ARM` reads

> Make the last CPU a shootdown waits for answer `arg` nanoseconds late, and
> take it away again.

The kernel arm no longer picks a CPU by arithmetic. `kernel/src/arch/tlb.rs`'s
`debug_arm_ack_delay` holds each other CPU's acknowledgement back for `arg`
nanoseconds in turn, takes one shootdown against each, and returns the smallest
wait any of them cost the initiator; the arming is then left standing against
every other CPU until a disarm or the end of a fresh `ARM_WINDOW_NANOS`
(`kernel/src/arch/tlb.rs:230`, two seconds), whichever comes first. So the one
sentence userland reads to learn what action 12 does describes a selection the
kernel does not make, omits the answer it returns, and says nothing about how
long what it leaves behind lasts.

`toyos-abi/src` is one of `toolchain::SYSROOT_SOURCES`, so the correction is a
single-commit branch of its own.

**Exit condition.** The doc names what the kernel does: each other CPU in turn,
the smallest of those waits returned, and the arming left standing against every
other CPU until a disarm or the end of the two-second window, whichever comes
first. A doc that names the arming without its window states a duration the
kernel does not provide.
