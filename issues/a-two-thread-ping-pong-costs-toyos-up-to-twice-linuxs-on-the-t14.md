---
status: open
kind: defect
opened: 2026-10-28
---

# A two-thread ping-pong costs ToyOS up to twice Linux's on the T14

`osbench sched`'s `pingpong.*` passes a turn between two threads, by futex
wait and wake (`futex`) and by a pipe's write and read (`pipe`). It runs on
one CPU (`same_cpu`) and across cores (`cross_core`). Each sample is one
round trip as the pinger sees it.

From the 2026-10-28 scoreboard (evidence branch `scoreboard-2026-10-28`):
ToyOS at 069cbbeb8, four boots off the internal NVMe, against five runs of
each bench's Linux as installed, interleaved. A cell is the p50 with its
run-to-run range. The AMD's Linux runs 1 to 3 timed through the TSC and 4
and 5 through the HPET, at 1.2 µs a timestamp.

| row | T14 ToyOS | T14 Linux | AMD ToyOS | AMD Linux |
|---|---|---|---|---|
| `pingpong.futex.same_cpu` | 4,580 ns (10.1%) | 2,279 ns (4.4%) | 5,015 ns (31.3%) | 6,248 ns (27.1%) |
| `pingpong.futex.cross_core` | 4,574 ns (2.5%) | 3,341 ns (0.9%) | 5,662 ns (26.7%) | 9,864 ns (43.5%) |
| `pingpong.pipe.same_cpu` | 6,186 ns (1.3%) | 4,504 ns (54.4%) | 5,656 ns (1.2%) | 7,739 ns (24.9%) |
| `pingpong.pipe.cross_core` | 6,802 ns (7.1%) | 3,712 ns (6.0%) | 5,746 ns (30.5%) | 15,021 ns (57.8%) |

On the T14 ToyOS loses all four, 1.37x to 2.01x, each resolved. On the AMD
ToyOS is faster in all four, and against Linux's TSC runs alone too: the
futex rows read 6,141 to 6,248 ns (`same_cpu`) and 9,648 to 9,864 ns
(`cross_core`) there, both above ToyOS's medians. The T14's Linux timed every
run through the TSC. Where its round trip's time goes, the wait, the wake or the
switch, is unmeasured.

#1074 has since changed the pipe on main and reads `pingpong.pipe.*` lower
on its head than its base
(`issues/a-64-kib-pipe-write-is-two-percent-below-the-two-mebibyte-ring.md`),
so the `pipe` rows' current state against Linux is owed a new reading.
The scheduler's pair-wake records bear on both kinds:
`issues/a-wake-held-for-its-posters-park-is-unpriced-on-metal.md` and
`issues/an-ordinary-wake-posted-on-its-own-cpu-waits-for-that-cpus-next-pass.md`.

**Current state, 2026-11-11** ([evidence](https://github.com/ToyOSOrg/evidence/tree/d44ebdae1f9f584d7b826e1498dbfd791a8d2339)): ToyOS at main ad5db5a65, four
boots a bench interleaved with five runs of its Linux as installed; a cell is
the median of the runs' p50s with their range, and the last column of each
bench is ToyOS against Linux, above 1 slower. The AMD bench's Linux timed
runs 1 to 3 through the HPET, so its medians overstate Linux's per-operation
times there.

| row | unit | T14 ToyOS | T14 Linux | T14 | AMD ToyOS | AMD Linux | AMD |
|---|---|---|---|---|---|---|---|
| `pingpong.futex.cross_core` | ns | 257 (1682.1%) | 3,346 (1.5%) | 0.08x | 4,634 (0.9%) | 13,410 (28.0%) | 0.35x |
| `pingpong.futex.same_cpu` | ns | 4,091 (1.0%) | 2,298 (3.4%) | 1.78x | 3,772 (0.8%) | 7,474 (18.7%) | 0.50x |
| `pingpong.pipe.cross_core` | ns | 5,758 (2.0%) | 3,697 (10.8%) | 1.56x | 5,506 (0.4%) | 14,457 (22.8%) | 0.38x |
| `pingpong.pipe.same_cpu` | ns | 4,957 (0.8%) | 3,774 (109.1%) | 1.31x | 4,774 (1.5%) | 9,011 (16.4%) | 0.53x |

On the T14 `futex.same_cpu` and `pipe.cross_core` are behind.
`pipe.same_cpu` is unresolved: Linux's five runs range 2.9 to 7.0 µs.
All four are ahead on the AMD bench.

**`futex.cross_core` did not measure a parked round trip in three of the
T14's four boots.** Their p50s read 257, 250 and 257 ns (minimum 213 to
215 ns); the third mixed both shapes in one run (p90 4,578, mean 1,188 ns),
as the AMD bench's fourth did (minimum 150, p50 4,629, mean 2,911 ns). A
round trip that short cannot hold a park: the same boots' `time.yield` alone
costs 652 ns and the same-CPU round trip 4,091 ns. The fourth boot read
4,573 ns, as all four did on 2026-10-28, which is 1.37x Linux's 3,346 ns:
where the row measured what it means, it is behind as before. That neither
thread slept in the short rounds is an inference from the timings; the row
counts no parks, so a run's p50 is not this row's value.

**Exit**: `pingpong.*` within Linux's on both benches in a labelled
current-state comparison, or each remaining difference measured to its parts
and justified; and `pingpong.futex.*` reports the parks of each round, so a
round trip that held none is not read as one.

**Owner**: the kernel's futex and wake path (`kernel/src/sched/futex.rs`,
`kernel/src/sched/`), and the pipe (`kernel/src/pipe.rs`) for its rows.
