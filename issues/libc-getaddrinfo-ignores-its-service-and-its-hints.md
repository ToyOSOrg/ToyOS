---
status: open
kind: defect
opened: 2026-10-08
---

# libc's getaddrinfo ignores its service and its hints

Read from the code, not run. `getaddrinfo` (`userland/libc/src/socket.rs`)
names its second and third arguments `_service` and `_hints` and reads
neither. Every answer's port is 0, so the usual C client,
`getaddrinfo(host, "80", &hints, &res)` and `connect(fd, res->ai_addr,
res->ai_addrlen)`, dials port 0. Every answer's `ai_socktype` is
`SOCK_STREAM` and its `ai_protocol` 0 whatever the hints ask, `AI_PASSIVE`
with a null node answers failure where POSIX has the wildcard address, and
`AI_NUMERICHOST` still asks the resolver. Every failure answers -1, which is
none of the `EAI_` codes `include/netdb.h` defines, and `gai_strerror` has
one text. A numeric host is read as `inet_pton` reads one, four decimal
numbers, where POSIX has `inet_addr`'s forms.

**Exit**: `getaddrinfo` answers the service's port, numeric or refused by
name, and the hints' socket type, protocol and flags, each failure is the
`EAI_` code POSIX names for it, and a guest C case connects to an address
and port `getaddrinfo` answered.

**Owner**: libc, under `issues/toyos-has-its-own-network-stack.md`: it
touches only `userland/libc` and a guest C case on `tests/netcase`.
