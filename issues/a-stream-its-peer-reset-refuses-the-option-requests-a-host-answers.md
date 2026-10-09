---
status: open
kind: defect
opened: 2026-10-08
---

# A stream its peer reset refuses the option requests a host answers

netstack lets a piped connection's socket and id go once the wire is finished
and it has closed both of its pipe ends (`bridge_piped`,
`userland/netstack/src/main.rs`). A peer's reset does both at once, so a
client that still holds the stream names an id netstack no longer has, and
`handle_tcp_set_option` answers it `ERR_NOT_CONNECTED`. Before the socket left
with its bridge it answered from the kept socket.

What a host answers `std::net::TcpStream` on a stream whose peer reset it,
after the read that reported the reset:

| | `set_nodelay(true)` | `nodelay()` |
|---|---|---|
| macOS (Darwin 27.0.0), measured | `Err(InvalidInput)`, `EINVAL` | `Ok(false)` |
| Linux, by the review of the change that made this, not measured | `Ok(())` | `Ok` |
| ToyOS, read from the code | `Err(NotConnected)` | `Ok`, the value last set |

So the write is refused as macOS refuses it, under another kind. The read asks
netstack nothing: std answers from the value its `TcpStream` holds
(`sdk/std/sys/net/connection.rs`) and libc's `getsockopt` from its socket's
entry (`userland/libc/src/socket.rs`). Keeping the id until the client's close
request is not the fix: a client that dies sends none, and netstack, having
closed its pipe ends, has nothing left that says the client is gone.

libc's `connect` asks the same request of a connection it has just been
answered, to hand over a `TCP_NODELAY` set before it, and its `accept` of one
it has just been given, to hand over its listener's: a peer that resets
between netstack's answer and that request makes either fail `ENOTCONN`, with
the connection closed, which no host's `connect` or `accept` answers.

**Exit condition**: a guest test on `tests/netcase` whose peer resets a stream
the client still holds, and `nodelay()` on it answers `Ok`, with
`netstack_socket_churn` still green; and the two compromises above ended: the
option request on a stream its peer reset is answered by netstack, and a libc
`connect` and a libc `accept` whose peer resets before the hand-over answer as
the host does, each held by a test that can be red.

**Owner**: whoever holds `issues/toyos-has-its-own-network-stack.md`.
