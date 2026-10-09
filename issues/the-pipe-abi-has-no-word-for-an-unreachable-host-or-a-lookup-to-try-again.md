---
status: open
kind: defect
opened: 2026-10-09
---

# The pipe ABI has no word for an unreachable host or a lookup to try again

netstack answers three of the node's endings in a word of the pipe ABI
(`toyos/src/net.rs`) that is not theirs (`userland/netstack/src/serve.rs`,
`connect_failed` and `Sockets::settle`):

- **A connect an ICMP error ended**, `Failure::Unreachable(_)` and
  `Failure::Prohibited`, is answered `ERR_OTHER`: std `Other`, libc `EIO`,
  with the ICMP code only in the log. Owed: network unreachable and host
  unreachable as std `NetworkUnreachable` / `HostUnreachable`, libc
  `ENETUNREACH` / `EHOSTUNREACH`; port unreachable as `ECONNREFUSED`; and
  `Prohibited` as `EHOSTUNREACH`, which is what Linux makes of an
  administratively filtered ICMP error.
- **A lookup ending `Unreachable`** is answered `ERR_NOT_CONNECTED`: std
  `NotConnected`, libc `ENOTCONN`, a stream's word and the wrong family for a
  lookup. Owed: network unreachable.
- **A lookup ending `LeaseChanged`** is answered the same. Owed: a word to try
  again, getaddrinfo's `EAI_AGAIN`.

No caller in the tree branches on any of the three today, so a program reads a
wrong reason and nothing acts on it.

**Exit condition**: an ABI change after the move gives the pipe ABI the words
above, std and libc map them as named, and netstack answers each ending in its
own word.

**Owner**: whoever next changes the pipe ABI (`toyos/src/net.rs`).
