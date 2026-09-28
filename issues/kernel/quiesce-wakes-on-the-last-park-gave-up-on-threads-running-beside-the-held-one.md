---
status: expected-red
kind: defect
opened: 2026-09-28
---

# `quiesce_wakes_on_the_last_park`'s stop gave up on threads still running beside the held one

Three Fast tiers carry the identical failure:

```
FAIL quiesce_wakes_on_the_last_park: the stop gave up on 2 thread(s) that never reached a safe point:
  stop: 3 of 5 userland thread(s) stopped across 2 cpu(s) in 2010 ms of a 2010 ms budget over 2 sweep(s), 0 of N userland block operation(s) still open; this reset lands wherever the other 2 are
```

- PR #539 at `2d6d228f`.
- PR #555 at `d2656765`.
- PR #559 at `ac948e6a`.

The earliest is PR #510 at `98e803cb`, recorded in
`issues/build/quiesce-wakes-on-the-last-park-lost-its-serial-ready-beside-other-guests.md`:
`stop: 4 of 7 userland thread(s) stopped ... in 2010 ms of a 2010 ms budget`.
That boot then lost its READY, so the harness reported the READY and not the
stop. None of the four branches touches the guest's stop path.

In the three sightings above, neither of the two threads was parked. Each
record has 0 block operations open, and `stop_if_blocked` stops every parked
thread outside a `block::OpenUpdate`, so the sweep counted both as running. One
of them is the held thread by construction: `quiesce::last::hold` yields until
the latest sweep counts 1 running, so a sweep that counts 2 keeps it spinning.
The defect is the other thread. No sighting can name it, because the `stop:`
record carries only counts.

**Hypothesis, untested.** The hold's yield loop keeps its CPU busy. A Ready
thread queued on that CPU then runs only if `dispose_yield`
(`toyos-sched/src/cpu.rs`) re-inserts the spinner behind it.

**What no enabled guest test checks while this is disabled.** Four of the six
quiesce guest tests are disabled: `quiesce_stops_the_machine`,
`quiesce_dump_holds_the_stopped`, `quiesce_wakes_on_the_last_exit` and this one.
`woken_by_its_threads` (`tests/common/power.rs`) has no enabled caller. So no
enabled test checks any of these:

- that a band, a park or an exit wakes the stop, rather than its deadline;
- `in_flight == 0` with `begun > 0`;
- the thread census;
- the `console-queue-at-the-stop` drain.

`quiesce_refuses_a_second_shutdown` stays green over a lost post. It judges the
stop only by `stopped_the_machine`, so a stop that spends its budget and then
finds everything stopped passes it.

**Exit**:

- an instrument that names each thread still running when the stop gives up,
  with its name, tid, cpu and scheduler state;
- the mechanism it names fixed;
- this test green in a Fast tier beside other guests;
- an enabled guest test checking each claim listed above.

Owner: the stop path, `kernel/src/quiesce.rs`; held by the orchestrator.
