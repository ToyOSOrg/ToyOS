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

**Exit**: `pingpong.*` within Linux's on both benches in a labelled
current-state comparison, or each remaining difference measured to its parts
and justified.

**Owner**: the kernel's futex and wake path (`kernel/src/sched/futex.rs`,
`kernel/src/sched/`), and the pipe (`kernel/src/pipe.rs`) for its rows.
