---
status: open
kind: defect
opened: 2026-10-28
---

# std's `yield_now` on ToyOS yields nothing

ToyOS has no yield syscall, and its std's `thread::yield_now` is a spin-loop
hint (`sdk/std/sys/thread.rs`). A Rust program that yields to let another
runnable thread go keeps its CPU instead, and spin-wait loops written against
`yield_now` behave unlike they do on every other platform. `osbench`'s
`time.yield` therefore reports `unsupported` on ToyOS rather than time a hint
against Linux's `sched_yield`.

From the 2026-10-28 scoreboard (evidence branch `scoreboard-2026-10-28`):
ToyOS at 069cbbeb8, four boots off the internal NVMe, against five runs of
each bench's Linux as installed, interleaved. A cell is the p50 with its
run-to-run range.

| row | T14 Linux | AMD Linux | ToyOS |
|---|---|---|---|
| `time.yield` | 190,677 ps (0.6%) | 552,167 ps (1.9%) | unsupported, all 4 boots on both benches |

**Exit**: a yield that gives the CPU to another runnable thread of the
caller's band, std's `yield_now` on it, and `time.yield` measured on
ToyOS within Linux's on both benches.

**Owner**: the scheduler's syscall surface (`kernel/src/sched/`, `sdk/abi`)
and ToyOS's std (`sdk/std/sys/thread.rs`).
