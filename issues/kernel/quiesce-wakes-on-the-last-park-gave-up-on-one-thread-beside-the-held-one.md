---
status: expected-red
kind: defect
opened: 2026-09-28
---

# `quiesce_wakes_on_the_last_park`'s stop gave up on one thread beside the held one

Three Fast tiers carry the identical failure:

```
FAIL quiesce_wakes_on_the_last_park: the stop gave up on 2 thread(s) that never reached a safe point:
  stop: 3 of 5 userland thread(s) stopped across 2 cpu(s) in 2010 ms of a 2010 ms budget over 2 sweep(s), 0 of N userland block operation(s) still open; this reset lands wherever the other 2 are
```

- PR #539 at `2d6d228f`.
- PR #555 at `d2656765`.
- PR #559 at `ac948e6a`.

`quiesce_wakes_on_the_last_teardown`'s boot shares the same shape, in the Fast
tier: PR #549 at `751e36d9`, `4 of 6 … 2010 ms of a 2010 ms budget over 2
sweep(s)`. That boot normally counts 5 userland threads.

The earliest is PR #510 at `98e803cb`, recorded in
`issues/build/quiesce-wakes-on-the-last-park-lost-its-serial-ready-beside-other-guests.md`:
`stop: 4 of 7 userland thread(s) stopped ... in 2010 ms of a 2010 ms budget`.
That boot then lost its READY, so the harness reported the READY and not the
stop.

One of the two threads is the held thread by construction: `quiesce::last::hold`
yields until the latest sweep counts 1 running, so a sweep that counts 2 keeps
it spinning. The defect is the other thread. No sighting can name it, because
the `stop:` record carries only counts.

**Hypothesis A, untested.** The hold's yield loop keeps its CPU busy. A Ready
thread queued on that CPU then runs only if `dispose_yield`
(`toyos-sched/src/cpu.rs`) re-inserts the spinner behind it.

**Hypothesis B, untested.** `tests/quiescelastcase/system.toml` starts `logd`,
which fsyncs `/log`. `begin_update`'s only caller is `SYS_FSYNC`
(`kernel/src/object/ops.rs:626`), and that update spans every retry, including
the park in `between_attempts`. A thread parked there is `Blocked` with
`MID_UPDATE`; `stop_if_blocked` refuses it (`toyos-sched/src/task.rs:450`), the
sweep counts it running, and it adds 0 to `in_flight` — so `logd` parked
between refused fsync attempts past the 2010 ms budget is a candidate the
records cannot exclude, and it sits on the same `/log` fsync path the writers
issue owns.

**What no enabled guest test checks while this is disabled.**
`woken_by_its_threads` (`tests/common/power.rs`) has no enabled caller. So no
enabled test checks any of these:

- that a band, a park or an exit wakes the stop, rather than its deadline;
- `in_flight == 0` with `begun > 0`;
- the thread census;
- the `console-queue-at-the-stop` drain;
- that the stop waits for a teardown in flight —
  `quiesce_wakes_on_the_last_teardown`'s only claim, and the only enabled
  guest check of it, disabled by this same issue.

`quiesce_refuses_a_second_shutdown` stays green over a lost post. It judges the
stop only by `stopped_the_machine`, so a stop that spends its budget and then
finds everything stopped passes it.

**Exit**:

- an instrument that names each thread still running when the stop gives up,
  with its name, tid, cpu and scheduler state;
- the mechanism it names fixed;
- this test green in a Fast tier beside other guests;
- `quiesce_wakes_on_the_last_teardown` — the stop waits for a teardown in
  flight — green in a Fast tier beside other guests;
- an enabled guest test checking each claim listed above.

Owner: the stop path, `kernel/src/quiesce.rs`; held by the orchestrator.
