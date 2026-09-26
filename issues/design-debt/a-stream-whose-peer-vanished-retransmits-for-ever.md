---
status: open
kind: defect
opened: 2026-09-26
---

# A stream whose peer vanished retransmits for ever

smoltcp 0.12 has no retransmission limit: an unacknowledged segment is resent
with its timeout doubling up to `RTTE_MAX_RTO` (10 s) and never given up on.
Its one bound, `Socket::set_timeout`, aborts on any silence of that length —
an idle connection with nothing to send included — so netd sets none on a
connection its client holds (`userland/netd/src/main.rs`, `PipedConnection`).

So a client writing to a peer that has gone — powered off, unplugged, behind a
NAT that forgot the mapping — is never told: its writes fill netd's buffer and
then the pipe, and then block, for ever unless the client set a write timeout
of its own. RFC 1122 §4.2.3.5 asks for the connection to be closed after R2
(at least 100 s) of retransmitting one segment; Linux does it after about 15
minutes (`tcp_retries2`). A connection whose client has let go of it is bounded
already, by netd's orphan limit.

Read from smoltcp's source and netd's; not reproduced by a test.

Exit condition: a stream whose oldest unacknowledged byte has gone unanswered
for R2 ends as a reset (its client's next read or write says so), with a test
whose peer vanishes mid-upload.
