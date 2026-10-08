---
status: open
kind: defect
opened: 2026-10-08
---

# netstack's datagram sockets and listeners have no bound

`max_piped_connections` (`userland/netstack/src/main.rs`) bounds piped TCP
connections and pending connects, and `resolve::MAX_LOOKUPS` the lookups in
flight. `handle_udp_bind` and `handle_tcp_bind_piped` check neither and
nothing else: every bind adds a socket with two 64 KiB buffers and holds the
client's 2 MiB pipes with it, two for a UDP socket and one for a listener. One holder of the `netstack`
connector that binds in a loop takes netstack's memory, and every dynamic UDP
port, from every other program. No bound is per client either:
`issues/netstack-lookup-slots-have-no-per-client-share.md` records that for
the two that exist.

Read from the code, not measured.

On the node (`userland/netstack/node/src/places.rs`) a datagram socket and a
listener each hold a place, and a bind or a listen with none left is refused
with nothing made: `a_datagram_socket_holds_a_place_and_a_bind_without_one_makes_nothing`
and `a_listener_holds_a_place_and_a_listen_without_one_makes_nothing`
(`userland/netstack/node/tests/listeners.rs`). The sockets of the node's own,
the responder's for the machine's name and each query's of a lookup, hold no
place: a lookup is bounded by `toyos_dns::MAX_LOOKUPS` and its rounds, so
clients at the bound refuse no lookup
(`a_query_with_no_port_to_leave_from_ends_its_lookup_by_name`,
`userland/netstack/node/tests/resolve.rs`). What is left: netstack as it
ships is the code above until it runs on the node; the node's places are one
number for every client, so its tests bind past the bound and see the refusal
but have no second client whose bind is answered, which the track records
with its own exit; and the word the pipe ABI answers a refused listen in is
the move's to map.

**Exit condition**: a bind past a stated bound is refused
`ERR_RESOURCE_EXHAUSTED` with nothing made, and a test binds past it and sees
the refusal and another client's bind answered.

**Owner**: whoever holds `issues/toyos-has-its-own-network-stack.md`.
