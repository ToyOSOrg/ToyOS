---
status: open
kind: defect
opened: 2026-09-26
---

# std's lookup_host answers a name with no address as `Other`

The std fork's `lookup_host` (`library/std/src/sys/net/connection/toyos.rs`)
answers netd's empty answer with `io::ErrorKind::Other` and the message
`DNS lookup failed: no results`, and every netd error that is not one of six
kinds with `Other` and `netd error`. A program asking for a name therefore
cannot tell a name that does not exist from a resolver that failed without
matching on the message, which is what `dns_resolve` in `tests/toyos.rs` does
(`DNS_NO_ADDRESS`). netd itself answers the two differently: an empty answer
for a name with none, and `ERR_OTHER` with a named line for a lookup that ended
without one (`userland/netd/src/main.rs`, `process_pending`).

Exit condition: a missing name and a failed lookup reach a std caller as two
different `ErrorKind`s, and `dns_resolve` reads the kind.
