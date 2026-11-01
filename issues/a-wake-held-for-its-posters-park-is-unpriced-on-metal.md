---
status: open
kind: defect
opened: 2026-10-12
---

# A wake held for its poster's park is unpriced on metal

A pass's opening holds every task it would forward for queueing behind the
running task, and places them after the pass's disposition
(`CpuSched::place_settled`, `kernel/pure/sched/cpu.rs`). A poster that parks,
stops or exits leaves the CPU to the first held task of each band; one that
yields or keeps the CPU forwards them to idle siblings as before, and so do
the rest of a burst. The simulator's `turn_taking_pair` holds the case: no
turn is forwarded off its CPU.

What it costs or saves on metal is unread. No row prices the forward against
the local switch. The first A/B that carried the rule, on the T14 with base
`0c7e62ca7` against a head that also carried CPU affinity and the spawn
tie-break, read twelve disk and memfs rows 1–3% slower (fstat, seek, stat,
pread_4k, read_4k_at, write_4k_at, tree.100), four boots a side with one image
each, so a layout difference between the images is not ruled out
([evidence](https://github.com/ToyOSOrg/evidence/tree/6cd2768011f0e6f36c4e2c1ce76f986fc6c2265b/pr-1116/r2)).
An A/B of the rule alone on the T14, base `24a538c88` against `042b2b299`,
reproduced seven disk rows 1.2–2.3% slower and left the ipc rows unchanged.
Timer rows the rule cannot reach (`wake.periodic.idle`, `tail.*`) read slower
by the same amount there and in two A/Bs of other changes, where the base
boots first and the images differ, so these A/Bs carry a head bias of that
size and the rows are not attributed
([evidence](https://github.com/ToyOSOrg/evidence/tree/f47a3a6c8324cc7c4a1f72921a350b2c2034dbec/pr-1111/r5)).

**Exit**: a T14 and an AMD A/B of the rule alone against its base, in an
order that boots each side first equally often, reads those rows within their
spread or names the cost, and a metal row prices a wake its poster parks
behind against the forward.
