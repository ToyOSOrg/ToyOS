---
status: open
kind: defect
opened: 2026-09-27
---

# An accept that never reaches netd strands its listener's owner

std's `TcpListener::accept` (`rust/library/std/src/sys/net/connection/toyos.rs`)
reads netd's wake byte first, and only then calls `toyos::net::tcp_accept`,
which reaches netd (`NetdConn::connect`) and makes the data path
(`DataPath::create`) before it sends the request. If either fails, or the send
does, the accept returns an error with the wake spent and no request made. netd
spends a wake only on an accept it answers (`userland/netd/src/listen.rs`), so
it still counts the owner as woken, writes no second wake, and the owner's next
`accept` blocks for the rest of the boot while the connection holds the port.

Read from the code and not reproduced. `NetdConn::connect` fails with
`ResourceExhausted` when the kernel's port queue refuses it, and
`DataPath::create` with `Io` when a pipe cannot be made.

**Owner**: whoever holds `issues/design-debt/toyos-has-its-own-network-stack.md`.

**Exit**: no failure between the wake and netd's answer leaves the owner
holding a spent wake netd still counts, and a test in which that step fails and
the next `accept` still returns the waiting connection.
