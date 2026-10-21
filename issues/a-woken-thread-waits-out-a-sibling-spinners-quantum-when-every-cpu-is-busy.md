---
status: open
kind: defect
opened: 2026-10-12
---

# A woken thread waits out a sibling spinner's quantum when every CPU is busy

With every CPU running a spinner of the woken thread's own process, a wake that
lands where it cannot be forwarded and cuts nothing waited for the pick, and the
pick served the fair band's frozen insertion-time keys: a queued sibling whose
key went stale when it was queued sits below the woken thread's, and two
threads of one process tie at the running one's. With the busy-CPU wake change
(PR #874) the PR's metal scout found phase 2's pair together on one CPU on both
benches at both judgements, which base's spawn rotation never did; how it got
there is not in the logs. One boot per tree each:

- **Phase 2** (a ping-pong pair and eight spinners of its process, eight CPUs):
  base 500 rounds in 5 s, head 250, on the T14 and the AMD bench, at both
  judgements. Every head wake was posted to the waker's own CPU and waited one
  quantum: max 10,003,344 ns on the T14 and 10,004,238 ns on the AMD bench.
- **Phase 3** (sixteen spinners): the AMD bench's woken p99 rose from
  ≤ 10,485,759 ns at base to ≤ 15,728,639 ns at the first judgement and
  ≤ 12,582,911 ns at the second, at equal rounds. The T14's rose from
  ≤ 16,777,215 ns to ≤ 29,360,127 ns at the first, and was lower than base's
  at the second.
- **Phase 5** (sixteen spinners of a second process): one T14 wake of 9,946
  waited 10,002,218 ns, against the protected run's 1 ms; the code's
  mechanism is the same pick, serving a queued spinner whose stale key sits
  below the woken thread's.

The logs: [first judgement](https://github.com/ToyOSOrg/evidence/tree/cfb4aa7c18348380fcb873e99809db20cf18af22/metal/judgement),
[second](https://github.com/ToyOSOrg/evidence/tree/d63b878f86d09c3aefcad27fd30eb3485fa81e5b/r2/metal/judgement).

The frozen insertion keys and the protected run are gone: each CPU runs EEVDF
over its job groups and their threads (`kernel/pure/sched/queue.rs`), and a
wake due first cuts the running request at its minimum run, `MIN_SLICE_NS`
(250 µs). What remains is the measurement.

**Before #874**, the scout behind it (`wt/toyos-scout-sys` at `fbb958b17`,
https://github.com/ToyOSOrg/ToyOS/pull/866#issuecomment-6119438569) timed a
ping-pong pair beside eight CPU-bound spinners on eight CPUs, post to
dispatch, as bucket upper bounds: on the T14 p50 ≤3.41 ms, p99 ≤6.82 ms,
max 6.69 ms; on the AMD bench p50 ≤2.62 ms, p99 ≤7.86 ms, max 7.55 ms. With no
core idle, such a wake is the fair band's pick to decide. In EEVDF (Stoica et
al., "A Proportional Share Resource Allocation Algorithm for Real-Time,
Time-Shared Systems", RTSS 1996) a task woken inside a quantum is picked no
sooner than the next quantum's start.

**Exit**: a re-run of K1–K4 on both benches, which reads phases 2, 3 and 5
against the base numbers above; and a metal row on the T14 and the AMD bench
with eight CPU-bound threads on eight CPUs that holds a woken task due first
to run within one minimum run and reports its p99.
