---
status: open
kind: defect
opened: 2026-09-27
---

# A handshake nobody finishes holds a listener's port shut

netd's listener is one smoltcp socket, and the port listens only while that
socket is in `Listen` (`userland/netd/src/listen.rs`). A SYN whose sender never
answers the SYN-ACK (a peer gone, a spoofed source) leaves it in `SynReceived`,
and smoltcp 0.12 retransmits the SYN-ACK without end.

Measured on smoltcp's interface over a wire played by hand, as
`userland/netd/src/listen/tests.rs` plays it: after one SYN and 600 s of
silence the socket was still `SynReceived`, having sent 72 SYN-ACKs, and
another peer's SYN in that state was answered with a reset. With
`set_timeout(10 s)` the socket went `Closed` at 10 s, which `listen::settle`
turns back into `Listen`.

So one packet shuts sshd's port for the rest of the boot. Nothing has chosen a
bound on a listener's half-open handshake, and the timeout smoltcp offers also
bounds the connection the socket becomes, so it would have to come off at the
hand-over.

**Owner**: whoever holds `issues/toyos-has-its-own-network-stack.md`,
which carries this and `issues/a-connect-between-two-accepts-is-reset.md`.

**Exit**: a listener's half-open handshake let go within a bound, and a test
that sends one SYN and nothing more, then finds the port answering the next
peer with a SYN-ACK.
