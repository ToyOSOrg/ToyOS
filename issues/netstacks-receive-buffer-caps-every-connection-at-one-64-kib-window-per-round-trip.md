---
status: open
kind: defect
opened: 2026-10-09
---

# netstack's receive buffer caps every connection at one 64 KiB window per round trip

netstack gives each connection a receive buffer of 65,535 bytes (`TCP_BUFFER`,
`userland/netstack/src/main.rs`), and the window it offers a sender is never
more than that buffer. `toyos-net-tcp` negotiates RFC 7323 window scaling, but
takes the smallest shift that fits the buffer in 16 bits (`Tcp::new`,
`toyos-net-shard/tcp/src/stack.rs`), which for this buffer is 0. A sender
then has at most 65,535 bytes in flight, and a download reaches at most
65,535 bytes per round trip, whatever the link: about 35 Mb/s at 15 ms,
5 Mb/s at 100 ms.

A larger buffer is paid in places: `PLACE_BYTES` counts a listener's
`LISTEN_READY` receive buffers and a stream's two, out of the eighth of memory
netstack's places may hold.

## Measured

The T14, `internet_download` at `b323e70ee`, one boot, the link up at
1000 Mb/s: `whole bytes=170439044 secs=38.826 mbps=35.1 busy=0.014`.
35.1 Mb/s is 65,535 bytes every 14.9 ms, and the CPUs were 1.4 % busy across
the transfer. That boot did not read the connection's round trip, so the
window is the ceiling by arithmetic and not yet by measurement; the job says
`rtt_ms`, each of its handshakes' times, from the next boot on. netstack exposes no connection's SRTT: its `inspect`
snapshot counts sockets and nothing of one.

## Owner

`issues/toyos-has-its-own-network-stack.md`.

## Exit

A T14 `internet_download` boot whose `mbps` exceeds 524.3 over the smallest
of its `rtt_ms` (65,535 bytes a round trip, in Mb/s).
