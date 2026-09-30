---
status: open
kind: tooling
opened: 2026-09-28
---

`hold`
panics the boot when the stop has not counted the held thread alone within it,
and `await_the_held_thread` panics when no thread named `quiesce-last` has
reached its syscall within it. `quiesce-last-park` and
`quiesce-last-teardown` all rest on it, and `quiesce_twice`'s `WAITS_WITHIN` is
priced inside it. A QEMU test's only clock is the harness's ceiling: a guest
slower than the deadline dies with a staging panic that names no defect.

Owner: orchestrator. Exit condition: both sides of the staging wait on the
other's event with no deadline, and a staging that never arrives is the
harness's ceiling.
