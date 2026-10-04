---
status: open
kind: defect
opened: 2026-09-26
---

# netd keeps receiving for a client that closed its receive end

When a piped TCP connection's client closes its receive pipe while the peer is
still sending, `bridge_piped` (`userland/netd/src/main.rs`) drops its end of
that pipe and nothing else: the socket stays open, keeps its unread bytes and
advertises a zero window, so the peer's further bytes sit in flight and its
sender blocks until the client also closes its send pipe — and even then the
socket only sends a FIN into a peer that is still trying to send. Nothing
tells the peer the bytes will never be read.

Pre-existing, and unchanged by the fix that stopped netd dropping bytes the
pipe had refused. `netd_refused_pipes`' fourth case builds exactly this state
and only asserts that netd survives it.

Exit condition: a connection whose receive end is gone with bytes still owed
is reset (or its receive side shut down) so the peer learns at once, with a
test whose host sender sees the connection end while the guest still holds its
send pipe.
