---
status: open
kind: defect
opened: 2026-09-25
---

# netd removes a closed stream before its FIN leaves

`handle_tcp_close` (`userland/netd/src/main.rs`) answers a client's close of a
TCP stream with `socket.close()` and then `socket_set.remove(handle)` in the
same pass. smoltcp's `close` only queues the FIN for the next `poll`, and the
socket is gone before that poll runs, so the FIN is never sent: the peer keeps
an established connection that nothing on this machine will ever write to
again, and learns otherwise only if it sends a segment and gets the reset back.

Read from the code, not measured. Every server that closes a connection it
accepted is affected — `logd` turning a reader away past its `MAX_READERS`,
sshd ending a session — and a peer that only reads, as the host's log reader
does, waits on it for ever.

Exit condition: a closed stream stays in the socket set until its FIN has been
sent (or the peer's reset has ended it), with a test whose host peer only reads
and sees the close.
