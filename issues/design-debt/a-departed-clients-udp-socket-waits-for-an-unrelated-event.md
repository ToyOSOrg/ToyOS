---
status: open
kind: defect
opened: 2026-09-26
---

# A departed client's UDP socket waits for an unrelated event

`NetDaemon::let_go_of_udp` (`userland/netd/src/main.rs`) closes a UDP socket
whose client has gone by asking its receive pipe with a zero-byte write, once
a pass. Nothing wakes a pass for that: a client with no receive pending is
watched by nothing, and its departure is no event netd is told of. So its
socket, its port and its two pipes stay held until some other client's
traffic, a lease renewal or a stream's deadline happens to run a pass.

Read from the pass; `netd_tcp_leaves` sees the socket let go because its own
next request runs the pass that notices.

Exit condition: a UDP socket whose client exits with no receive pending is let
go of without another event, with a test that waits on netd's count while
sending netd nothing.
