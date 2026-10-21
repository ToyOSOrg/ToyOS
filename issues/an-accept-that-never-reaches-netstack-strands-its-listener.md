---
status: open
kind: defect
opened: 2026-09-27
---

# An accept that never reaches netstack strands its listener's owner

std's `TcpListener::accept` (`sdk/std/sys/net/connection.rs`) reads
netstack's wake byte first, and only then calls `toyos::net::stream::accept`,
whose `open` (`sdk/toyos/src/net/stream.rs`) reserves a slot in the process's
stream table, makes the stream's region (`SharedMemory::create`) and reaches
netstack (`NetstackConn::connect`) before it sends the request. If any of
those fails, or the send does, the accept returns an error with the wake spent
and no request made. netstack spends a wake only on an accept it answers
(`Node::accept`, `system/netstack/node/src/listeners.rs`), so it still
counts the owner as woken, writes no second wake, and the owner's next
`accept` blocks for the rest of the boot while the connection holds the port.

Read from the code and not reproduced. The table refuses past `MAX_STREAMS`
with `ResourceExhausted`, as the region does when no 8 MiB of 2 MiB pages is
free (`issues/a-stream-commits-eight-mebibytes-and-a-process-holds-128.md`),
and `NetstackConn::connect` fails with `ResourceExhausted` when the kernel's
port queue refuses it.

**Owner**: whoever holds `issues/toyos-has-its-own-network-stack.md`.

**Exit**: no failure between the wake and netstack's answer leaves the owner
holding a spent wake netstack still counts, and a test in which that step fails and
the next `accept` still returns the waiting connection.
