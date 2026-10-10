---
status: open
kind: defect
opened: 2026-10-09
---

# A listener's TCP_NODELAY has no setter in std or the forks, and a C program cannot order the waiting case

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

`tests/host.rs` is green on Linux too: the `host` check runs it on
`ubuntu-24.04`, whose log names
`a_host_gives_a_connection_the_nodelay_its_listener_had_when_it_began ... ok`.

The pipe ABI carries both halves (`toyos/src/net.rs`): a bind's request
carries its listener's options and `MsgType::TcpListenerSetOption` sets one
afterwards, and an accept's answer says what its connection holds
(`TcpOptions`). netstack answers the rule for both by the node's calls
(`userland/netstack/src/serve.rs`, `userland/netstack/node/src/listeners.rs`),
and the `libc_sockets` guest test reads it: `nodelay_accepted` by `toyos::net`, for a connection that
waited when the option was set, its wake read first, and for the next one
dialled; `tests/netcase/nodelay_kept.c` by libc's `setsockopt` and
`getsockopt`, for a listener's set before `bind`, its set and its clear after,
and the connections dialled after each. What no program reads yet:

- A C program cannot set the option while a connection waits and know that
  one does: libc's `poll` watches kernel handles and a socket descriptor is
  none (`userland/libc/src/posix_io.rs`), so the only call that says a
  connection waits is the `accept` that takes it. `nodelay_kept.c` therefore
  holds the half of the rule a second dial orders, and not the first.
- No Rust program can set the option on a listener. `std::net::TcpListener`
  has no call for it on any host, where a program reaches the socket through
  socket2 by its descriptor. On ToyOS, read from the forks at the commits the
  lock names and not run: socket2 (`1d90250`, `src/sys/toyos.rs`) keeps a
  table of its own sockets, so a std listener's descriptor names none of
  them; on a listener of its own its `setsockopt` of `TCP_NODELAY` answers
  `Ok` and hands the option to nobody, and its `accept` records the accepted
  socket's as off whatever the answer says; its bind names no option
  (`toyos::net::tcp_bind`, which `tcp_bind_with` stands beside only until
  the fork's bind passes its own). mio (`d1edcaa`,
  `src/net/tcp/toyos_stream.rs`) answers `nodelay()` true for every stream
  and its `set_nodelay` sends nothing. std's `TcpListener::accept`
  (`sdk/std/sys/net/connection.rs`) takes the accepted stream's option from
  the answer, which no test can turn red while no std listener can hold one.

The node (`userland/netstack/node/src/listeners.rs`) takes a listener's
option in its `listen` and in `Node::set_listener_nodelay`, and answers an
accept with what its stream's connection took from its listener; nothing ships
it until the move.

**Exit condition**: on ToyOS a program that sets `TCP_NODELAY` on a
listening socket and accepts one connection begun before and one begun after
reads the option on each as that test has a host answer, by libc's
`getsockopt`, which needs libc's `poll` to answer for a listening socket, and
by std's `nodelay()`, which needs a listener a Rust program can set the option
on: the socket2 fork's, with the lock naming the commit that hands its
listener's option to netstack in its bind and reads its accepted socket's from
the answer, and mio's that reads a stream's; and `toyos::net` has one bind
call, which takes its options.

**Owner**: whoever holds `issues/toyos-has-its-own-network-stack.md`; the
fork commits are on socket2's and mio's `toyos` branches.
