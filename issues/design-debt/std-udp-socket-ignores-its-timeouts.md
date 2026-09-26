---
status: open
kind: defect
opened: 2026-09-26
---

# std's UdpSocket ignores its timeouts

The ToyOS net pal in the std fork (`library/std/src/sys/net/connection/toyos.rs`)
stores `UdpSocket::set_read_timeout` and `set_write_timeout` and answers them
back, but `recv_from` and `send_to` never read either: a `recv_from` with a
read timeout set waits for a datagram forever. A Rust program that bounds a
UDP exchange the ordinary way is not bounded. `netd_udp_any_address` runs its
receive on a thread and bounds the thread instead for this reason.

Exit condition: `recv_from` returns `WouldBlock`/`TimedOut` once the read
timeout passes, and a guest test with no answering peer shows it.
