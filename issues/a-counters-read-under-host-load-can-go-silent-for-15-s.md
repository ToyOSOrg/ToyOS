---
status: open
kind: defect
opened: 2026-10-04
---

# A counters read under host load can go silent for 15 s on an 8-CPU `virt` guest

`test_rs_counters_read` in `tests/virtsmpcase` normally ends about 0.2 s of
guest clock after `unmap_touch` does (median 0.17-0.20 s over 570 guests, max
1.46 s). Under host load it sometimes takes seconds, and it has gone silent
long enough for the harness's 15 s quiet bound to call the guest stalled: each
silent boot is below.

`virt_mask_windows` boots the same case on a `mask-windows` kernel and waits
on two things, the job `unmap_touch` and then the boot's last word, so a
silence in the counters read reds it as `STALLED: waiting for the boot's last
word`: that wait spans the read, `test_rs_trace_read` and the shutdown. Its
stalls are recorded here.

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
  second profile and brings the exit no closer.

The third and fourth silent ones, two guests of one run: `virt_el1_smp` and
`virt_mask_windows`, in the whole suite on `wt/toyos-libc-sockets` at
`68cf1f870`, whose diff touches no line of either wait and nothing these
guests start: `tests/virtsmpcase` runs no C program, opens no socket and
starts no netstack.

- **The lines.** `FAIL virt_el1_smp: STALLED: waiting for the job
  test_rs_counters_read to end — it went quiet` and `FAIL virt_mask_windows:
  STALLED: waiting for the boot's last word — it went quiet`, both at the same
  second of the host's clock; 35 of 37 passed.
- **The load.** `--jobs 4 --nocapture` on the 14-core host, 1-minute load
  75.99 when the run began and 63.37 when it ended, from other worktrees'
  builds; liveness ceilings paid at 1.52x; workers 366 s building against
  808 s testing.
- **The waits.** `virt_el1_smp`: `judge_virt_job`'s `await_marker` on
  `===TEST_END test_rs_counters_read `. `virt_mask_windows`: its second wait,
  `await_marker` on `power::SHUTTING_DOWN`, after `judge_virt_job` had judged
  `unmap_touch`. Each was ended by `GUEST_QUIET`.
- **The same profile as the second.** Each guest's last three lines are the
  end of `unmap_touch`, the start of the read and the kernel's record of its
  spawn, with no thread's exit after it. `virt_el1_smp`: `===TEST_END
  unmap_touch exit=0===` at 13.577, `===TEST_START test_rs_counters_read===`
  at 13.578, `spawn: /system/bin/test_rs_counters_read pid=13` at 13.590 on
  cpu4. `virt_mask_windows`: the same three at 14.557, 14.558 and 14.585, on
  cpu4. The two spawn records reached the harness within one second of each
  other, and nothing followed from either for the 15 s.
- **What is known.** `virt_smp`, the same case launched one second before
  `virt_el1_smp`, did the step in the same seconds and passed: the read began
  at guest 13.936 and ended at 19.120, five seconds of the host's clock.
  `virt_failed_ap_leaves_no_hole`, launched one second after, ends at
  `unmap_touch` and does not wait on the read. Each red, run alone at the
  same commit, passed in 4 s, at loads 32.47 and 37.04.
- **What is not known.** Whether the guests ran, spun or were not scheduled:
  no register capture exists. Two guests going silent at the same step in the
  same second is the first occurrence that bears on the host stopping the
  guest, and it shows no more than that. The serial directory the harness
  named for that run was not kept; the run's log, outside the tree, is the
  only copy.

Two other stalls of `virt_mask_windows` have no console of the guest, so
the step each stopped in is not read from it:

- **`wt/toyos-move-abi` at `94fd25e71`.** `FAIL virt_mask_windows: STALLED:
  waiting for the boot's last word — it went quiet`, ten minutes after the
  two at `68cf1f870` by the two runs' logs, with nothing in what the
  guest said while it was waited on, `STALL virt_mask_windows (38s)`; the
  whole suite at `--jobs 4`, 34 of 36, workers 1794 s building against 703 s
  testing. Load 59.30 69.33 70.48 when the suite began and 85.73 69.02 67.23
  when it ended, 14 cores; fastest boot 2765 ms against the reference
  1424 ms, ceilings at 1.94x. The wait is the second one, ended by
  `GUEST_QUIET`, which no host factor stretches. Known: `unmap_touch` had
  ended with exit 0, its line reaching the harness 15 s before the verdict,
  the guest named by elimination among the three of that case then running.
  In the same run `virt_smp` said its `unmap_touch` line at guest 21.786 and
  its `counters_read` line at 34.278, 13 s apart on the host's clock and 2 s
  inside the bound; `virt_el1_smp`, earlier in that run, 13.481 and 15.702.
  The branch changes nothing that guest's kernel or harness is built from.
  At `c95d0ffc0` the test passed in the whole suite at load 12.82 rising to
  36.76 and alone at 36.76 to 42.53, neither at the red's load. Not known:
  whether the guest was alive or what it did in those 15 s; the red run's
  kept serial directory holds eleven UART logs, each an x86-64 boot, and no
  PL011's. Whether the silence is `virt_smp`'s 13 s grown past 15 under the
  hooks' cost, or a guest that stopped. It was not run again at that commit,
  nor on `main` at that load.
- **`wt/toyos-acpi1` at `b9430ed61`.** The same line after 45 s, in the whole
  suite; workers 19038 s building against 1375 s testing, load 82.65 83.62
  77.89 moments after it, 14 cores. Nothing in that branch runs on that
  boot. Alone at that load and commit it passed in 14 s. On `main` at
  `d47b383cf`, `cargo test -- virt_mask_windows` passed three times: alone at
  load 45 rising to 72 (10 s), and twice beside a whole suite at load 74 to
  77 (38 s, with ceilings paid at 8.00x and 31 s between its image and its
  first guest line) and 75 to 67 (7 s).

The fork compiler's ScalarEvolution fault (llvm/llvm-project#175729, which
`src/miscompile.rs` now refuses a sysroot for) is not their cause.
`tests/virtsmpcase`'s image was built and `virt_el1_smp` run with the fixed
compiler and with the fault switched back on
(`-C llvm-args=-scev-unconditional-preinc-nowrap-flags`), and every function
compared in IR and in object code: `counters_read`, `test-runner`, `supervisor`,
`logkeeper`, `toybox`, `kernelprobe` and the loader are byte-identical between
the two, and the kernel differs in seven functions of `rustc_demangle`'s `v0`
printer, a loop peeled or not, which a counters read does not call.

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

Owner: the kernel's AArch64 bring-up, `issues/toyos-runs-on-arm64.md`, held
by the orchestrator's next kernel worker.
