---
status: open
kind: defect
opened: 2026-10-08
---

# libc reads an IPv4 address in host order

`sockaddr_in`'s `s_addr` is in network byte order: its four bytes in memory
are the address's four octets. libc's `parse_sockaddr`
(`userland/libc/src/socket.rs`) takes them with `s_addr.to_be_bytes()`, which
on a little-endian machine reverses them, and `fill_sockaddr`, `getaddrinfo`
and `inet_addr` write `u32::from_be_bytes(ip)`, reversed the same way. So an
address libc made itself comes back right, and one a program made as POSIX
says does not: `inet_pton` copies the octets in order and `htonl` swaps, and
`connect` reads either backwards.

Measured in a guest on `tests/netcase`, x86-64, with a host server listening:
`connect` to `{htonl(0x0a000202)}` answered -1, and to
`{inet_addr("10.0.2.2")}` answered 0.

**Exit**: every address libc reads or writes is its octets in memory order,
and a guest C case connects to an address built with `htonl` and to one built
with `inet_pton`.
