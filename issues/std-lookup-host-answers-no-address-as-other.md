---
status: open
kind: defect
opened: 2026-09-26
---

# std's lookup_host answers a name with no address as `Other`

std's `lookup_host` (`sdk/std/sys/net/connection.rs`)
answers netstack's empty answer with `io::ErrorKind::Other` and the message
`DNS lookup failed: no results`, and a netstack refusal that is
`ResourceExhausted`, `Protocol` or `Io` with `Other` and `netstack error`
(`net_err_to_io`). A program asking for a name therefore cannot tell a name
that does not exist from a resolver that failed without matching on the
message. netstack itself answers the two differently: an empty answer for a
name with none, and a refusal for a lookup that ended without one.

Exit condition: a missing name and a failed lookup reach a std caller as two
different `ErrorKind`s.
