---
status: open
kind: defect
opened: 2026-10-14
---

# A released function never claimed again keeps its retired grants

`kernel/src/pcidev/mod.rs`'s `release` resets a function whose holder ended
and parks that holder's grants in `Function::retired`, mapped in its domain
until the reset is quiet, so nothing the function had issued lands in a page
handed on. A function that leaves the device set has them freed when it is
retired (`retire`): nothing behind a slot that lost it can write any more.
A function that stays, reset, released and never claimed again keeps up to
`MAX_GRANT_TOTAL` (32 MiB) of grant pages for the rest of the boot, since only
its next claim frees them (`finish_retired`). Across the machine that is
bounded only by the functions released and not reclaimed.

Freeing them once the reset is quiet needs work to run at a deadline on
nobody's thread, which the kernel has no way to ask for: the deferred
callback `issues/hotplug-blocks-a-scheduler-pass.md` names.

Exit condition: that callback exists, a released function's retired grants
are freed by it once its reset is quiet, whether or not it is claimed again,
and a test that releases a reset-capable function and never claims it again
sees the pages go back.
