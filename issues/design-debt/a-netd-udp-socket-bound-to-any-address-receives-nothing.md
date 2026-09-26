---
status: open
kind: defect
opened: 2026-09-26
---

# A netd UDP socket bound to any address receives nothing

`handle_udp_bind` (`userland/netd/src/main.rs`) binds the smoltcp socket to
`IpEndpoint::new(addr, port)` whatever `addr` the client sent. smoltcp 0.12
turns an `IpEndpoint` into a listen endpoint with `addr: Some(addr)`
(`wire/ip.rs`, `From<Endpoint> for ListenEndpoint`), and its UDP `accepts`
drops every unicast datagram whose destination is not that address. So a
client that binds `0.0.0.0` — the ordinary way to receive on any address —
gets a socket that sends and never receives; only broadcast and multicast
reach it. No test received a UDP datagram before `netd_udp_refused`, which
binds the leased `10.0.2.15` for this reason: bound to `0.0.0.0`, its first
receive went unanswered for its whole 20 s bound (EXIT=1); bound to
`10.0.2.15`, the same program is green (EXIT=0).

Exit condition: an unspecified address binds with `addr: None`, and a guest
test receives a unicast datagram on a socket bound to `0.0.0.0`.
