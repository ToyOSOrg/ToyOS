---
status: open
kind: defect
opened: 2026-09-26
---

# A listener whose peer closes before `accept` is never replaced

`NetDaemon::handle_tcp_accept_piped` (`userland/netd/src/main.rs`) hands its
owner the listener's socket only once that socket is `Established`, and makes
the replacement listener only then. A peer that connects and sends its FIN
before the owner accepts leaves the socket in `CLOSE-WAIT`, so every `accept`
from then on is refused `ERR_NOT_CONNECTED`, and the port listens for nobody
until its owner closes the listener.

Read from the handler; not reproduced by a test.

Exit condition: an `accept` after a peer connected and closed before it
answers that connection (with its bytes and its end) or the next one, with a
test whose host connects, writes, and shuts down before the guest accepts.
