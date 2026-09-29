---
status: open
kind: tooling
opened: 2026-09-29
---

# The `isa` port switch's cost on the scheduler hot path is unmeasured

`arch::pio::switch_to` (`kernel/src/arch/x86_64/pio.rs`) runs on every context
switch, from `KernelHw::switch`: per `GRANTABLE` row one `BOUND` load, one
bitmap byte read, and on a change one bitmap write per port. On x86-64 that is
one row on every switch of every CPU, whether or not any process holds a claim.
Nothing has measured what it adds to a switch, and a timing verdict comes only
from metal.

**Exit**: the T14's switch cost with and without the call, from one metal run,
recorded in the commit that closes this; or the switch skips the rows entirely
while no row is bound, with the skip's cost measured the same way.
