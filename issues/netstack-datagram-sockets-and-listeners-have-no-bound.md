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

**Exit condition**: a bind past a stated bound is refused
`ERR_RESOURCE_EXHAUSTED` with nothing made, and a test binds past it and sees
the refusal and another client's bind answered.

**Owner**: whoever holds `issues/toyos-has-its-own-network-stack.md`.
