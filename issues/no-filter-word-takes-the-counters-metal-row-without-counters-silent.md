---
status: open
kind: tooling
opened: 2026-10-31
---

# No filter word takes the counters metal row without counters_silent

`tests/common/metal.rs`'s `select` takes every row and shared member whose
name contains a filter word. Every substring of `counters`, the row's whole
name, is also part of the shared member `counters_silent`, which rides
`shared-debug`. An offline run (`--metal --metal-readback <dir> counters`)
judges only once every selected boot has a readback. So judging the
`counters` row over a `testcases` readback alone stages `shared-debug`
instead, and nothing is judged.

#1100's review had to drop `counters_silent` from the selection with a
temporary patch to judge round 1's T14 readback (evidence pr-1100 r8,
`judge-select.patch`). Any row whose name is part of another row's or
member's name has the same problem.

**Exit**: a filter word can select one row alone, for example an exact-name
form, and the `counters` row is judged offline over a `testcases` readback
with no patch.
