---
status: open
kind: defect
opened: 2026-09-26
---

# std's `TcpStream::local_addr` answers an address nobody asked netd for

The ToyOS net pal in the std fork (`library/std/src/sys/net/connection/toyos.rs`,
`TcpStream::socket_addr`) answers `10.0.2.15` and the stream's local port.
The port is netd's; the address is a literal, the one QEMU's user network
leases. netd takes its address from DHCP, so on any other network — the
bench's router among them — a Rust program asking which address its
connection left from is told one the machine does not hold. It is the same
literal `getsockname` answers in libc
(`issues/design-debt/getsockname-answers-an-address-nobody-asked-netd-for.md`),
and it needs the same missing answer from netd: `toyos::net` has no call
that says which address a socket is bound to.

Nothing in the tree reads the address today; `netd_tcp_ports` reads only the
port.

Exit condition: `local_addr` answers the address netd's socket is bound to,
from netd, with a guest test on a network whose lease is not `10.0.2.15`.
