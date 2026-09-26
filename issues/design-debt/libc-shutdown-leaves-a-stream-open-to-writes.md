---
status: open
kind: defect
opened: 2026-09-26
---

# libc's `shutdown(SHUT_WR)` leaves a stream open to writes

netd queues a stream's FIN behind every byte in its send pipe and sends it
once the pipe is empty (`userland/netd/src/stream.rs`, `fin_after_drain`).
std refuses a write after `shutdown(Write)` itself; libc's `shutdown`
(`userland/libc`) asks netd and refuses nothing after, so a C client that
keeps writing keeps the pipe from emptying and defers its own FIN for as long
as it writes — the stream it asked to half-close stays open.

Read from libc and netd; no test writes through the pipe after a shutdown.

Exit condition: a write after `shutdown(SHUT_WR)` through libc fails with
`EPIPE`, with a C test beside std's `half_close`.
