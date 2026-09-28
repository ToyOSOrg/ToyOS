---
status: open
kind: tooling
opened: 2026-09-28
---

# Which pass `drain_irqs` is told it is entered from is gated only by a nightly guest

`kernel-loom`'s `only_a_pass_entered_at_depth_zero_takes_the_request` holds
`Entered::may_serve`, the decision. The two call sites that choose its argument,
`kernel/src/sched/driver.rs`'s `pass` (`Entered::Pass` at the depth it was
entered at) and `pass_block` (`Entered::Blocking`), are in no crate a host test
compiles. PR #564's round-2 review handed `pass_block`'s drain
`Entered::Pass { depth: 0 }`: every host test and the fast tier stayed green,
and only the nightly `blocked_dump` went red.

**Exit**: a mutation of either call site's `Entered` reds a host test or a
fast-tier test. Owner: orchestrator.
