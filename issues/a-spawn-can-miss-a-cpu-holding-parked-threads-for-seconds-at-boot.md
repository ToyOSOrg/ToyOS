---
status: assigned
kind: defect
opened: 2026-10-28
---

# A spawn can miss a CPU holding parked threads for seconds at boot

`osbench`'s `wake.onto_busy` (`tests/guest/osbench/src/wake.rs`) places
partner threads on one CPU, where each partner parks. `place::spawn_on`
spawns threads until one first runs on that CPU, with a 5 s budget per
partner. The rows that measured took 32–63 ms. In some T14 `benchcase`
boots the row instead ran for seconds, inside about 22.7–29.0 s of boot,
the span in which netstack's DHCP lease and `ready` line also land. Four
rows refused, one of them on main, for example "partner_33_was_not_placed:
138904_threads_started_in_5s_and_none_first_ran_where_it_was_wanted", and
`osbench_sched` exited 1.

T14 `benchcase` boots, all on 2026-10-28:

| images | boots | refused | slow but measured |
|---|---|---|---|
| main `544f9cc3c`: PR #1074's session 1 base-bench, sessions 3–5 base1/base2, and its diagnostic base image (`544f9cc3c` with measurement counters) in the first diagnostic session and diag2 and diag3 base1/base2 | 12 | 1: diag3 base2 (23.362–28.364 s) | 1: session 4 base2's row ran 23.156–27.025 s (3.87 s), around netstack's `ready` at 24.99 s |
| PR #1074's heads: session 1 head-bench (`1f828f331`), session 2 p1 (`46848c491`), sessions 3 (`5e0cb52b0`), 4 (`cd42d13a4`) and 5 (`0a48bf8b4`) head1/head2, and its diagnostic images: `cd42d13a4`, `0a48bf8b4`'s merge, diag2's pool1/pool2 and diag3's skew1/skew2 | 14 | 3: session 2 p1 (23.963–28.967 s), session 5 head1 (22.705–27.710 s), the `0a48bf8b4` merge image (22.802–27.803 s) | — |

Main `544f9cc3c` refuses too, so the behaviour predates #1074. Whether #1074
raises the refusal rate is not established: 1 of 12 against 3 of 14,
one-sided Fisher exact p = 0.36. The readbacks are PR #1074's sessions 1–5
and its diagnostic sessions 1–3. diag2's first base2 attempt is left out:
that boot never came back, because the I219 was left in ULP.

**The cause is the bench's premise.** `CpuHandles::place`
(`kernel/pure/sched/cpu.rs`) takes, among the CPUs still answering, the least
`load`, then a sleeping CPU, then the fewest busy SMT siblings. A slow boot of
a diagnostic image (`370d7d9f6` with placement counters, T14, diag2
boot 3) shows the spawner running on the wanted CPU itself: the
kernel's cpu1, which osbench named by APIC ID 2. About 7,200 spawns per 250 ms
window were made from cpu1, none of them chose it while another CPU slept, and
no CPU stopped answering, had a kick pending or had anything queued. A
candidate reached cpu1 only through a rare wake forward of a parked one. The
scheduler followed its rule; what was false is `place::spawn_on`'s premise
that spawns rotate over the CPUs.

**The fix is the bench's.** osbench and `wake_onto_busy` now pin every
placed thread through `toyos_abi::syscall::thread_set_affinity`, and the
staging is gone.

Owner: the scheduler track (`sched-place`).

Exit: every `wake.onto_busy` row in 20 consecutive main `benchcase` boots
completes within 250 ms.
