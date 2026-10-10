---
status: open
kind: defect
opened: 2026-10-08
---

# netstack's places are one number for every client

Every stream, listener and datagram socket a client makes netstack hold takes
one of the node's places (`userland/netstack/node/src/places.rs`), which
netstack sets from an eighth of physical memory at what a listener can be made
to hold (`userland/netstack/src/main.rs`, `places_for`), and a connect, a
listen, an accept and a bind with none left are refused with nothing made:
`a_connect_past_the_places_is_refused_and_sends_nothing`,
`a_listener_holds_a_place_and_a_listen_without_one_makes_nothing` and
`a_datagram_socket_holds_a_place_and_a_bind_without_one_makes_nothing`
(`userland/netstack/node/tests/listeners.rs`), and in a guest the connect past
them, answered `ERR_RESOURCE_EXHAUSTED` (`netstack_socket_churn`). The sockets
of the node's own, the responder's for the machine's name and each query's of
a lookup, hold no place: a lookup is bounded by `toyos_dns::MAX_LOOKUPS` and
its rounds.

The places are one number for every client. One holder of the `netstack`
connector that connects, listens or binds in a loop takes them all, and every
dynamic UDP port, from every other program:
`issues/netstack-lookup-slots-have-no-per-client-share.md` records the same of
the lookups, and why netstack cannot share them out itself.

**Exit condition**: a test takes one client to its share of the places, sees
its next bind refused `ERR_RESOURCE_EXHAUSTED` with nothing made, and another
client's bind answered.

**Owner**: whoever holds `issues/toyos-has-its-own-network-stack.md`.
