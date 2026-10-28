---
status: open
kind: defect
opened: 2026-10-28
---

# A futex ping-pong costs ToyOS up to twice Linux's on the T14

`osbench sched`'s `pingpong.futex.*` passes a turn between two threads by
futex wait and wake, on one CPU (`same_cpu`) and across cores
(`cross_core`). Each sample is one round trip as the pinger sees it.
The 2026-10-28 scoreboard (ToyOS at 069cbbeb8, 4 boots off the internal NVMe against 5 runs of each bench's Linux as installed, interleaved; p50s, with the run-to-run range; evidence branch `scoreboard-2026-10-28`):

| row | T14 ToyOS | T14 Linux | AMD ToyOS | AMD Linux |
|---|---|---|---|---|
| `pingpong.futex.same_cpu` | 4,580 ns (10.1%) | 2,279 ns (4.4%) | 5,015 ns (31.3%) | 6,248 ns (27.1%) |
| `pingpong.futex.cross_core` | 4,574 ns (2.5%) | 3,341 ns (0.9%) | 5,662 ns (26.7%) | 9,864 ns (43.5%) |

On the T14 ToyOS loses both, 2.01x on one CPU and 1.37x across cores, and
both resolve. On the AMD ToyOS resolves better. That bench's Linux timed
two of five runs through the HPET, at 1.2 µs a timestamp. The pipe
ping-pong rows show the same T14 shape (`pingpong.pipe.same_cpu` 1.37x,
`cross_core` 1.83x). Where the round trip's time goes, the wait, the wake or
the switch, is unmeasured.

**Exit**: `pingpong.futex.*` within Linux's on both benches in a labelled
current-state comparison, or each remaining difference measured to its parts
and justified.

**Owner**: the kernel's futex and wake path (`kernel/src/sched/futex.rs`,
`kernel/src/sched/`).
