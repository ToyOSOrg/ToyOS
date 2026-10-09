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

The T14, `internet_download` at `83ef5a9ae`, one boot, the link up at
1000 Mb/s: `whole bytes=170439044 secs=38.657 mbps=35.3
rtt_ms=[16.8 15.7 15.0 16.0 16.5] busy=0.016`, judged `PASS`. One window per
round trip is 524.3 / `rtt_ms` Mb/s: 31.2 to 35.0 across the five handshakes,
and the rate sits at the top of that range with the CPUs 1.6 % busy across the
transfer. Each `rtt_ms` is one handshake, a round trip plus netstack's own time
for a connect, so it bounds the path's round trip from above and 524.3 over it
bounds the window's ceiling from below: a rate held at the ceiling clears that
bound by up to the handshakes' spread, 12 % in this boot. netstack exposes no
connection's SRTT: its `inspect` snapshot counts sockets and nothing of one.

The T14 sits on a gigabit connection, wired to its router, by the owner's
statement; the line's own rate is not measured. The goal is close to that:
"No bar but its a gigabit connection and i would like close to that" (the
owner, asked the line's speed).

## Owner

`issues/toyos-has-its-own-network-stack.md`.

## Exit

Both of:

- A host test in `toyos-net-tcp`, at netstack's stream buffer size, that a
  stream's SYN carries a window shift above 0 and that the window it
  advertises with an empty receive buffer exceeds 65,535 bytes. A buffer of
  65,535 bytes fails both.
- A T14 `internet_download` reading after the fix, recorded here with its
  `mbps` and `rtt_ms`. It carries no rate threshold: the owner set "No bar but
  its a gigabit connection and i would like close to that".
