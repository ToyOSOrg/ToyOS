---
status: expected-red
kind: defect
opened: 2026-09-28
---

# `quiesce_wakes_on_the_last_park` gave up on two threads that never reached a safe point

Three orchestrator Fast-tier runs, each on a branch that touches no quiesce
code, carry the identical failure:

```
FAIL quiesce_wakes_on_the_last_park: the stop gave up on 2 thread(s) that never reached a safe point:
  stop: 3 of 5 userland thread(s) stopped across 2 cpu(s) in 2010 ms of a 2010 ms budget over 2 sweep(s), 0 of N userland block operation(s) still open; this reset lands wherever the other 2 are
```

`539r7-fast.log` (`Compiling toyos-build v0.1.0 (/Users/jan/Dev/jan/toyos-install7)`,
`Running tests/toyos.rs (target/debug/deps/toyos_build-eac527f418717d9c)`), block
count 294. `555r2-fast.log` (`Compiling toyos-build v0.1.0
(/Users/jan/Dev/jan/toyos-reap)`, `Running tests/toyos.rs
(target/debug/deps/toyos_build-2ff7bec9bcd9b25c)`), block count 263.
`559-fast.log` (`Running tests/toyos.rs
(target/debug/deps/toyos_build-2ff7bec9bcd9b25c)`), block count 331.

This is a different shape than
`issues/build/quiesce-wakes-on-the-last-park-lost-its-serial-ready-beside-other-guests.md`,
whose sighting is `QEMU died before ===READY===` with an empty uart after a
stop that reported every thread stopped; here the stop itself reports two
threads still parked and gives up.

**Exit**: the two threads' safe-point wait explained on a loaded host, and
the test green in a Fast tier beside other guests. Owner: the stop path,
`kernel/src/quiesce.rs`; held by the orchestrator.
