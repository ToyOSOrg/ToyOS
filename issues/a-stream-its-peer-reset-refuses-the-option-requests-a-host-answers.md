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
`handle_tcp_set_option` and `handle_tcp_get_option` answer it
`ERR_NOT_CONNECTED`. Before the socket left with its bridge both answered from
the kept socket.

What a host answers `std::net::TcpStream` on a stream whose peer reset it,
after the read that reported the reset:

| | `set_nodelay(true)` | `nodelay()` |
|---|---|---|
| macOS (Darwin 27.0.0), measured | `Err(InvalidInput)`, `EINVAL` | `Ok(false)` |
| Linux, by the review of the change that made this, not measured | `Ok(())` | `Ok` |
| ToyOS, read from the code | `Err(NotConnected)` | `Err(NotConnected)` |

So the write is refused as macOS refuses it, under another kind, and the read
is refused where both hosts answer. Keeping the id until the client's close
request is not the fix: a client that dies sends none, and netstack, having
closed its pipe ends, has nothing left that says the client is gone.

**Exit condition**: a guest test on `tests/netcase` whose peer resets a stream
the client still holds, and `nodelay()` on it answers `Ok`, with
`netstack_socket_churn` still green.

**Owner**: whoever holds `issues/toyos-has-its-own-network-stack.md`.
