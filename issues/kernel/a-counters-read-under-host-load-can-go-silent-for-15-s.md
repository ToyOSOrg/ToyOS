---
status: open
kind: defect
opened: 2026-10-04
---

# A counters read under host load can go silent for 15 s on an 8-CPU `virt` guest

`test_rs_counters_read` in `tests/virtsmpcase` normally ends about 0.2 s of
guest clock after `unmap_touch` does (median 0.17-0.20 s over 570 guests, max
1.46 s). Under host load it sometimes takes seconds, and once it went silent
long enough for the harness's 15 s quiet bound to call the guest stalled.

The silent one: `virt_el1_smp`, 8 vCPUs under TCG, on a 14-core host at
1-minute load 38-53 while the primary checkout was building the stage-2
compiler; the branch `wt/toyos-counterstall` at `bc5f36c7b`, whose harness
keeps every line the guest said. `test_rs_counters_read` started at 4.031,
spawned at 4.044, two of its threads exited (4.174 and 4.576), and then the
console carried nothing until the harness gave up 15 s later. No CPU printed
`sched: cpu=` again, though each last printed one at 2.17-2.22 s and prints
again on its first idle trip 10 s on (`scheduler::log_health`): no CPU went
idle, or the console stopped. No register capture of that guest exists. One
in 30 guests of that loop; none in the 1724 guests that followed on the same
host at load 15-60.

The slow ones, with registers: a probe that captured `info registers -a` over
QMP whenever the read had not ended 3 s after `unmap_touch` caught one (span
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

Exit: the cause of the silence is named from a capture of a silent guest
(registers over QMP before anything else touches it), and fixed with the
evidence, or shown to be the host stopping the guest.
