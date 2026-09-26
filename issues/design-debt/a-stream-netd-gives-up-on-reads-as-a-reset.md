---
status: open
kind: defect
opened: 2026-09-26
---

# A stream netd gives up on reads as a reset, not a timeout

A stream whose peer stops acknowledging is ended after `STALL_LIMIT`
(`userland/netd/src/stream.rs`), and its client learns it the only way the
pipes can say anything but EOF: the send pipe closes first, which std reads as
`ConnectionReset`. Linux answers the same stream `ETIMEDOUT`; a client that
retries on a timeout and gives up on a reset takes the wrong branch here.

Exit condition: a client's read or write on a stream netd gave up on answers
`TimedOut`, with `netd_tcp_vanish`'s writer asserting that kind.
