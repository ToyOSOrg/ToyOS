---
status: open
kind: defect
opened: 2026-10-28
---

# A spawn onto a busy CPU can miss it for seconds at boot

`osbench`'s `wake.onto_busy` (`tests/guest/osbench/src/wake.rs`) places
partner threads on one busy CPU through `place::spawn_on`. That spawns
threads until one first runs on the wanted CPU, within a 5 s budget per
partner. Usually the row takes 30–60 ms. Sometimes, early in the T14's
`benchcase` boot (22–28 s, around netstack's DHCP lease and its `ready`
line), one partner's placement runs for seconds: 138,904 threads in 5 s
and none first ran where it was wanted. The row refuses, so `osbench_sched`
exits 1.

T14 `benchcase` boots, all on 2026-10-28:

| image | boots | refused | slow but under 5 s |
|---|---|---|---|
| main `544f9cc3c`, and its diagnostic image | 8 | 0 | 1 (session 4 base2: 3.9 s, 23.156–27.025 s, netstack `ready` at 24.99 s) |
| PR #1074's heads and diagnostic images | 9 | 3 | — |

The slow placement happens on `544f9cc3c`, so it predates #1074. Whether
#1074 makes a refusal likelier is not established (one-sided p ≈ 0.09). The
spawn path the row takes (`vm_reserve`, `vm_protect`, `thread_spawn`, and a
std channel) touches no pipe. The readbacks are in PR #1074's sessions 2–5
(`tests/toyos.rs`'s `osbench` row).

What makes spawn placement avoid one CPU for seconds is not known.
`issues/placement-has-no-migration-cost-or-wake-affinity.md` says a spawn
prefers a sleeping CPU on a free core. The wanted CPU holds the parked
partners, and netstack's bring-up is busy at the same time.

Owner: the scheduler track,
`issues/placement-has-no-migration-cost-or-wake-affinity.md`.

Exit: the cause is named from a boot log that shows the spawn's choices
while it misses, and either fixed or held by a bounded placement test. Then
`wake.onto_busy` measures in at least 20 consecutive `benchcase` boots of
main.
