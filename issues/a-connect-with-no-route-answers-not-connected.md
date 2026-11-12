---
status: open
kind: finding
opened: 2026-11-11
---

# A connect with no route answers not connected

Found in the review of #1227. netstack answers a connect that the stack
refuses for want of a route (`connect_refused` in `system/netstack/src/serve.rs`,
`ConnectError::Route`) with `ERR_NOT_CONNECTED`, by design: the word clears
when a lease lands. libc therefore says `ENOTCONN`, and std `NotConnected`.
POSIX's `connect` page has no `ENOTCONN` for this. Its word for no route to
the network is `ENETUNREACH`, which the pipe ABI now carries as
`ERR_NETWORK_UNREACHABLE`.

osbench's lookup retry already treats `NetworkUnreachable` as a lease still
landing, so moving the word costs callers nothing there. Any other caller
that keys on `NotConnected` to wait for a lease has to be found first.

**Exit condition**: a connect with no route answers `ERR_NETWORK_UNREACHABLE`,
with every caller that waits for a lease keyed on that word, shown by a
netstack host test of `connect_refused`.
