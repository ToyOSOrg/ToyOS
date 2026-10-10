---
status: open
kind: defect
opened: 2026-09-26
---

# netd keeps receiving for a client that closed its receive end

When a stream's client closes its receive pipe while the peer is still
sending, the node lets its end of that pipe go and nothing else
(`userland/netstack/node/src/streams.rs`, `Stream::pass`): the connection
stays open, [tcp] keeps its unread bytes and advertises a closing window, so
the peer's further bytes sit in flight and its sender blocks until the client
is done writing too. Then the node closes the connection, and [tcp] resets one
closed with text unread (`a_close_with_text_unread_resets`,
`userland/netstack/node/tests/streams.rs`). Until then nothing tells the peer
the bytes will never be read.

Read from the code, not measured.

Exit condition: a connection whose receive end is gone with bytes still owed
is reset (or its receive side shut down) so the peer learns at once, with a
test whose host sender sees the connection end while the guest still holds its
send pipe.
