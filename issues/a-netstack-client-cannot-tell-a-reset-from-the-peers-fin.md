---
status: open
kind: defect
opened: 2026-09-26
---

# A netstack client cannot tell a reset connection from the peer's FIN

A stream reaches its client as two pipes, and the only thing the receive pipe
can say about the end of the stream is EOF: its write end closed. std reads
the rest off the send pipe (`sdk/std/sys/net/connection.rs`, `ended`): a send
pipe with no reader behind a receive pipe at its end is a reset, on the rule
that netstack ends the send pipe too, and first, when a connection did not end
in order.

The node ends both pipes for a reset (`a_reset_ends_both_pipes_and_the_stream`,
`userland/netstack/node/tests/streams.rs`), so a reset is read as one. It also
lets the send pipe go at an orderly end of the client's writing, once the last
byte is queued and the FIN after it, and whenever the connection is over
(`userland/netstack/node/src/streams.rs`). So a client that shut its sending
half down and then reads its peer's bytes to the end reads `ConnectionReset`
where the peer sent a FIN, and so does one whose connection finished both ways
in one pass. Measured in a guest on the first run of `netstack_streams`, whose
job then read to the end: every one of its 4,194,304 bytes came back as sent,
and the read after the last answered `connection reset`, on virtio and on the
e1000e alike. netstack on smoltcp kept the send pipe until its socket closed,
which for a client that had sent its FIN first was after the peer's.

The pipe carries no word for which it was, so std cannot tell the two apart by
looking harder: either the node keeps the send pipe of a connection that ends
in order until its client has read the end, or std stops reading a send pipe
it shut itself, which leaves a reset after a shutdown read as a FIN.

**Exit condition**: a guest test whose client shuts its sending half down,
reads its peer's echo and then the end as an end; one whose peer resets a
stream reads the reset as a reset; and neither is told
by a rule that the other ending can also meet.

**Owner**: whoever holds `issues/toyos-has-its-own-network-stack.md`.
