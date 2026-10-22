---
status: open
kind: defect
opened: 2026-09-26
---

# netstack keeps receiving for a client that shut its receive half

A stream's `shutdown(Read)` sets the client's own `read_shut` and nothing else
(`toyos::net::stream::shutdown_read`, `sdk/toyos/src/net/stream.rs`): every read
then answers 0, but netstack is not told. It keeps taking the peer's bytes into
the region's receive ring, [tcp] advertises a closing window as the ring fills,
and the peer's further bytes sit in flight while its sender blocks, until the
client hangs up. Then the node closes the connection, and [tcp] resets one
closed with text unread (`a_client_that_leaves_with_text_unread_resets`,
`system/netstack/node/tests/streams.rs`). Until then nothing tells the peer the
bytes will never be read.

Read from the code, not measured.

Exit condition: a stream whose client shut its receive half with bytes still
owed is reset (or its receive side shut down at the peer) so the peer learns at
once, with a node test in which the peer sees the connection end while the
client still holds its send half.
