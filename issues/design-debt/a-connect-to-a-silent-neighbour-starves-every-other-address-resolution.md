---
status: open
kind: defect
opened: 2026-09-26
---

# A connect to a silent on-link neighbour starves every other address resolution

smoltcp 0.12's neighbour cache (`iface/neighbor.rs`) rate-limits ARP with one
`silent_until` for the whole cache, and a socket whose neighbour is missing is
silenced for the same second (`socket_meta.rs`, `DISCOVERY_SILENT_TIME`). A TCP
connect to an on-link address nothing answers — a host that is off, a typo'd
LAN address — retries its ARP request every second, and because
`socket_egress` visits sockets in slot order, the older socket wins the rate
limit every time: every later socket whose neighbour is not already cached is
answered `RateLimited` and silenced again, for as long as the silent connect
lives.

Measured on netcase: a thread's `toyos::net::tcp_connect(10.0.2.99, 9, 0)`
(slirp answers ARP for its own addresses only), then 300 ms later a
`TcpStream::connect_timeout` to the host at `10.0.2.2` with nothing cached for
it: the second connect ends `TimedOut` after its whole 60 s, reproduced twice
of two. With the host's address already cached, the same second connect
succeeds at once — which is what `netd_tcp_leaves` arranges, and what every
connection through the router has only while the router's entry is fresh
(smoltcp's `ENTRY_LIFETIME`, 60 s from the last frame it saw from it).

Upstream has the milder half of this as smoltcp-rs/smoltcp#1209 (open): one
address's request delays another's by up to a second. The starvation is that
issue plus the fixed visiting order. The fix it suggests — a silence per
address, falling back to the cache-wide one only when the table is full —
removes both.

Exit condition: a connect whose neighbour never answers does not delay
another address's resolution past that address's own rate limit, with a guest
test that connects to the host beside a silent connect and nothing cached.
