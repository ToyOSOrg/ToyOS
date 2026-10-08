---
status: open
kind: defect
opened: 2026-10-08
---

# The socket2 fork's SO_BROADCAST setter does nothing

Read from the fork at the commit the lock names (`1d90250`), not run. On
ToyOS, socket2's `setsockopt` (`src/sys/toyos.rs`) matches
`(SOL_SOCKET, SO_BROADCAST)` to an empty arm and answers `Ok`, and its
`getsockopt` answers 0 for it: a setter that says yes to a permission it hands
to nobody, where std's `UdpSocket::set_broadcast` and libc's `setsockopt` send
`toyos::net::udp_set_option`. No send follows it today: the backend holds no
datagram socket at all, so a program that reaches a datagram socket through
socket2 has none on ToyOS.

**Exit**: the fork's ToyOS backend either carries a datagram socket whose
`SO_BROADCAST` reaches netstack and reads back, or refuses the option by name
on a socket it cannot send a datagram from; the lock names the fork commit
that does.

**Owner**: whoever holds `issues/toyos-has-its-own-network-stack.md`; the
change is a commit on the socket2 fork's `toyos` branch.
