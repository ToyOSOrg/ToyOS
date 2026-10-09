---
status: open
kind: defect
opened: 2026-10-09
---

# A handshake reset before it ends hands its option to the next connection

netstack on smoltcp gives a connection the `TCP_NODELAY` its listener held
when its SYN arrived (`userland/netstack/src/listen.rs`), which is a host's
rule (`userland/netstack/node/tests/host.rs`), except for the connection that
follows a handshake its peer reset before it ended.

Read from the two sources, not run. A listener there is one smoltcp socket. A
set of the listener's option is written to that socket only while it is in
`Listen` (`Listening::set_nodelay`); once a SYN has arrived the set is kept in
`Listening` for the socket that listens next, which `Listening::open` gives it
after an accept, or after a reset that left the socket `Closed`
(`Listening::settle`). smoltcp 0.12.0 puts a socket that takes an RST in
`SynReceived` straight back to `Listen` (`src/socket/tcp.rs`, the
`(State::SynReceived, TcpControl::Rst)` arm: `self.tuple = None;
self.set_state(State::Listen)`), with the Nagle switch it had. No pass of
netstack sees that socket `Closed`, so nothing writes the listener's option to
it again.

The sequence: the listener holds the option, a SYN arrives, the owner clears
the option, the peer resets, as a SYN scan does. The socket listens again with
Nagle's algorithm off, and the next connection begins with `TCP_NODELAY` at a
listener that held none when its SYN arrived; its accept answers the option
on. The mirror gives a connection without the option at a listener whose
`getsockopt` reads 1. A set that reaches the socket while it listens again, or
the accept that replaces it, ends the difference: one connection a reset
handshake is affected, if the owner changed the option during that handshake.

No test holds it, and none is to be built: the owner's ruling is that no new
test is built on smoltcp. The node, which replaces this stack, has the rule
for this case: [tcp] keeps the options with the listener and copies them to a
connection when its SYN arrives (LS-10), a reset handshake leaves nothing
behind, and
`a_handshake_reset_before_it_ends_leaves_the_next_connection_its_listeners_option`
(`userland/netstack/node/tests/listeners.rs`) plays the sequence both ways
round and reads the next connection's option from the accept's answer.

**Exit condition**: netd runs on the node and `userland/netstack/src/listen.rs`
and smoltcp are deleted, with that test of the node green.

**Owner**: whoever holds `issues/toyos-has-its-own-network-stack.md`, at the
move.
