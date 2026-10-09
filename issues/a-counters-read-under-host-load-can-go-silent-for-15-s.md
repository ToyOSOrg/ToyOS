---
status: open
kind: defect
opened: 2026-10-04
---

# A counters read under host load can go silent for 15 s on an 8-CPU `virt` guest

`test_rs_counters_read` in `tests/virtsmpcase` normally ends about 0.2 s of
guest clock after `unmap_touch` does (median 0.17-0.20 s over 570 guests, max
1.46 s). Under host load it sometimes takes seconds, and twice it went silent
long enough for the harness's 15 s quiet bound to call the guest stalled.

The first silent one: `virt_el1_smp`, 8 vCPUs under TCG, on a 14-core host at
1-minute load 38-53 while the primary checkout was building the stage-2
compiler; the branch `wt/toyos-counterstall` at `bc5f36c7b`, whose harness
keeps every line the guest said. `test_rs_counters_read` started at 4.031,
spawned at 4.044, two of its threads exited (4.174 and 4.576), and then the
console carried nothing until the harness gave up 15 s later. No CPU printed
`sched: cpu=` again, though each last printed one at 2.17-2.22 s and that
kernel printed again on a CPU's first idle trip 10 s on: no CPU went idle, or
the console stopped. No register capture of that guest exists. One
in 30 guests of that loop; none in the 1724 guests that followed on the same
host at load 15-60.

The second silent one: `virt_smp` (EL2, started through SMC), the first on
that profile, on `wt/toyos-shortstop` at `0c0a348e1`, whose diff touches no
line of the wait and none the guest runs.

- **The line.** `FAIL virt_smp: STALLED: waiting for the job
  test_rs_counters_read to end — it went quiet`.
- **The load.** 14 tests 12 wide on the 14-core host, 1-minute load 30.40 when
  the run began and 50.09 when it ended, from other worktrees' builds;
  liveness ceilings paid at 4.98x; workers 1440 s building against 1072 s
  testing.
- **The wait.** `judge_virt_job`'s `await_marker` on
  `===TEST_END test_rs_counters_read `, ended by `GUEST_QUIET`, which is 15 s
  of wall clock and is not paid out for host speed or guest width as
  `GUEST_WEDGED` is.
- **What differs from the first.** The silence begins straight after the
  job's first spawn record, with no thread's exit on the console: the guest's
  last three lines are `===TEST_END unmap_touch exit=0===` and
  `===TEST_START test_rs_counters_read===` at 13.724 and the kernel's
  `spawn: /system/bin/test_rs_counters_read pid=13` at 13.737, and nothing
  reached the harness in the 15 s of wall clock after them.
- **What is known.** `virt_el1_smp`, the same case booted one second later
  beside it, did the same step in about one second of wall clock (the job's
  spawn at guest 14.757, its end at 15.908) and had exited eleven seconds
  before the verdict; the run's twelve other tests had ended earlier still,
  so for the last eleven seconds of the silence no other guest of that run
  was alive. `virt_smp` alone at load 52.19 passed in 9 s, the job ending
  0.4 s of guest clock after its start, and in the same group of 14 at load
  24.75 it passed: one silent boot in the five of that case at that commit.
- **What is not known.** Whether the guest was running, spinning or not being
  scheduled by the host, and whether it was the guest or its console that
  stopped. No register capture of it exists either, so it adds a count and a
  second profile and brings the exit no closer. The fork compiler miscompiles,
  for AArch64, an inclusive range that ends at its integer type's maximum;
  whether that reaches this job's code has not been examined.

The slow ones, with registers: a probe that captured `info registers -a` over
QMP whenever the read had not ended 3 s after `unmap_touch` (`debug-slow.patch`
in https://github.com/ToyOSOrg/ToyOS/pull/719#issuecomment-5978643928, which
also carries `debug-regs.patch`, the capture of a stalled one) caught one (span
3.68 s, `virt_el1_smp`, load 38) in 174 guests. Every CPU was at EL1 with
DAIF masked; four (cpu0, 3, 5, 7) were in `Lock::lock`'s ticket spin on
`counters::ANSWERED`'s waiter list (x19 its address in the shipping kernel's
symbols), two at the kick handler's `ICC_EOIR1_EL1` write, one inside the
post. 500 ms later all were back in `driver::pass`, and the read finished.
Earlier slow reads without registers spanned 7, 15 and 26 s.

Waking a round's readers once, from the answer that completes it, instead of
once per answering CPU (`762a524f0`, reverted in the next commit) changed
neither the kicks taken during the read (median 116/122/118, fix/base/fix)
nor its span, so the waiter-list contention is not shown to be the cause.

**The idle report is gone.** The owner ruled on 2026-10-04, choosing "Remove
it entirely": "Delete the periodic report and its counters; hang triage uses
the trace diary and panic records instead." A silent guest no longer says
whether its CPUs went idle by a `sched:` line's absence; its diary
(`kernel/src/trace.rs`, read by `/system/bin/trace`) and its panic records
(`kernel/src/panic.rs`, `kernel/src/blackbox.rs`) do.

Exit: the cause of the silence is named from a capture of a silent guest
(registers over QMP before anything else touches it), and fixed with the
evidence, or shown to be the host stopping the guest.
