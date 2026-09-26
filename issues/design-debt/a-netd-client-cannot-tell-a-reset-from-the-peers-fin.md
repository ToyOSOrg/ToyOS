---
status: open
kind: defect
opened: 2026-09-26
---

# A netd client cannot tell a reset connection from the peer's FIN

A piped TCP connection reaches its client as two pipes, and the only thing
the receive pipe can say about the end of the stream is EOF: its write end
closed. `bridge_piped` (`userland/netd/src/main.rs`) closes that end for
every ending alike — the peer's FIN, the peer's RST, and netd resetting the
connection itself through `PipedConnection::refuse` (a pipe whose ring page
could not be allocated, or a handle netd cannot write or read). The client
reads the same zero-byte `read` in all of them, so a stream cut short by a
reset is indistinguishable from one the peer finished, and a protocol with no
length framing of its own takes a truncated stream as a whole one.

The `ResourceExhausted` case is reachable without a hostile client: the ring
page is allocated on a pipe's first write, and a peer that speaks first (an
ssh banner) makes netd's first write the allocation. The only record is
netd's own log line.

Read from the code, not measured.

Exit condition: the pipe protocol carries a reset as something other than
EOF — for instance an error the client's next `read` returns — and a test
whose connection netd resets sees that error rather than EOF.
