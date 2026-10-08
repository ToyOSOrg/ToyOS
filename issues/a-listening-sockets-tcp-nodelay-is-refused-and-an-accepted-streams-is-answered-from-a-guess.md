---
status: open
kind: defect
opened: 2026-10-08
---

# A listening socket's TCP_NODELAY is refused, and an accepted stream's is answered from a guess

A host takes `TCP_NODELAY` on a listening socket and gives it to the
connections that begin afterwards. Read on macOS 27 on arm64, by libc's
`setsockopt` and `getsockopt` over loopback: set before the connection began,
the accepted socket has the option; set after the connection was established
and before the accept, it does not. `userland/netstack/node/tests/host.rs`
asks the same of every host the suite runs on, and sets the option in the
second case only once the listener is readable, which is the host saying the
connection waits to be accepted: a connect returns before the listener's end
is established, so the answer rests on that event and not on the order
loopback delivers in. A red there prints the host's name and both answers.

ToyOS as it ships does neither half:

- netstack answers `TcpSetOption` and `TcpGetOption` `ERR_NOT_CONNECTED` for
  any id that is not a stream (`handle_tcp_set_option`,
  `userland/netstack/src/main.rs`), and libc's `setsockopt` sends a listening
  socket's id there (`userland/libc/src/socket.rs`), so a C program that sets
  the option on its listener is refused.
- std builds every `TcpStream`, an accepted one included, with
  `nodelay: false` and answers `nodelay()` from that field without asking
  netstack (`sdk/std/sys/net/connection.rs`). Nothing can make that false
  today, since no accepted connection has the option. Once a listener's
  option reaches the connections it accepts, it is false for each of them.

The node (`userland/netstack/node/src/listeners.rs`) has the calls,
`Node::set_listener_nodelay` and `Node::listener_nodelay`, and gives an
accepted stream what its connection took from its listener; nothing ships it.

Not read: Linux. No Linux host was at hand when this was filed. The `host`
check runs `tests/host.rs` on Linux, and its first run on a pull request that
carries the file is that reading.

**Exit condition**: `tests/host.rs` is green in a `host` check's log, which
is the Linux reading; and on ToyOS a program that sets `TCP_NODELAY` on a
listening socket and accepts one connection begun before and one begun after
reads the option on each as that test has a host answer, by libc's
`getsockopt` and by std's `nodelay()`.

**Owner**: whoever holds `issues/toyos-has-its-own-network-stack.md`; the
Linux reading is the orchestrator's, at the first `host` run of the pull
request that adds `tests/host.rs`.
