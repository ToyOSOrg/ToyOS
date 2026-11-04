---
status: open
kind: tooling
opened: 2026-10-28
---

# ToyOS's yield is not yet measured against Linux's

std's `thread::yield_now` on ToyOS is `SYS_YIELD` (`sdk/std/sys/thread.rs`),
which gives the CPU to another runnable thread of the caller's band, and
`osbench`'s `time.yield` times it on ToyOS as on Linux
(`tests/guest/osbench/src/time.rs`). Until it did, std's `yield_now` was a
spin-loop hint, and the 2026-10-28 scoreboard (evidence branch
`scoreboard-2026-10-28`) recorded the row unsupported:

| row | T14 Linux | AMD Linux | ToyOS |
|---|---|---|---|
| `time.yield` | 190,677 ps (0.6%) | 552,167 ps (1.9%) | unsupported, all 4 boots on both benches |

**Exit**: `time.yield` measured on ToyOS on both benches beside their
Linux, and within it, or the difference named.

**Owner**: the orchestrator's next scoreboard run.
