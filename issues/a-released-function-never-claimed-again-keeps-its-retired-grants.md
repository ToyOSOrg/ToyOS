---
status: open
kind: defect
opened: 2026-10-14
---

# A released function never claimed again keeps its retired grants

`kernel/src/pcidev/mod.rs`'s `release` resets a function whose holder ended
and parks that holder's grants in `Function::retired`, mapped in its domain
until the reset is quiet, so nothing the function had issued lands in a page
handed on. Only that function's next claim frees them (`finish_retired`). A
function reset, released and never claimed again keeps up to
`MAX_GRANT_TOTAL` (32 MiB) of grant pages for the rest of the boot. Across
the machine that is bounded only by the functions released and not reclaimed.

Owner: `issues/a-device-function-is-driven-from-its-arrival-to-its-end.md`,
whose stage 2 ends a function's claim with `Gone`.

Exit condition: a released function's retired grants are freed once its
reset is quiet, whether or not it is claimed again, and a test that releases
a reset-capable function and never claims it again sees the pages go back.
