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
and before the accept, it does not. `system/netstack/node/tests/host.rs`
asks the same of every host the suite runs on, and sets the option in the
second case only once the listener is readable, which is the host saying the
connection waits to be accepted: a connect returns before the listener's end
is established, so the answer rests on that event and not on the order
loopback delivers in. A red there prints the host's name and both answers.

`tests/host.rs` is green on Linux too: the `host` check runs it on
`ubuntu-24.04`, whose log names
`a_host_gives_a_connection_the_nodelay_its_listener_had_when_it_began ... ok`.

The protocol carries both halves (`sdk/toyos/src/net/socket.rs`): a listen's
request carries its listener's options and its holder's `SET_OPTION` sets
one afterwards, and an accept's answer says what its connection holds
(`TcpOptions`). netstack answers the rule for both by the node's calls
(`system/netstack/src/serve.rs`, `system/netstack/node/src/listeners.rs`),
and the node takes a listener's option in its `listen` and in
`Node::set_listener_nodelay`, and answers an accept with what its stream's
connection took from its listener. The `libc_sockets` guest test that read it
(`nodelay_accepted` by `toyos::net`, `tests/guest/c/nodelay_kept.c` by libc's
`setsockopt` and `getsockopt`) was cut in `ff1a11dbd`. What no program reads:

- Whether a C program can set the option while a connection waits and know
  that one does. libc's `poll` now watches a listener by its holder's bell
  (`sdk/libc/src/posix_io.rs`); no test reads that it answers a waiting
  connection.
- No Rust program can set the option on a listener. `std::net::TcpListener`
  has no call for it on any host, where a program reaches the socket through
  socket2 by its descriptor. On ToyOS, read from the forks at the commits the
  lock names and not run: socket2 (`12abd8f`, `src/sys/toyos.rs`) holds std's
  `TcpListener` for a listening socket, binds it through
  `TcpListener::bind`, which carries no option, answers a listener's
  `setsockopt` of `TCP_NODELAY` `Ok` and sets nothing, and its `getsockopt`
  0; an accepted socket is std's `TcpStream`, whose option it reads. mio
  (`836d14f`) delegates a stream's option to std's. std's
  `TcpListener::accept` (`sdk/std/sys/net/connection.rs`) takes the accepted
  stream's option from the answer, which no test can turn red while no std
  listener can hold one.

**Exit condition**: on ToyOS a program that sets `TCP_NODELAY` on a
listening socket and accepts one connection begun before and one begun after
reads the option on each as that test has a host answer, by libc's
`getsockopt`, with libc's `poll` answering for the waiting listener, and by
std's `nodelay()`, which needs a listener a Rust program can set the option
on: the socket2 fork's, with the lock naming the commit that hands its
listener's option to netstack in its listen.

**Owner**: whoever holds `issues/toyos-has-its-own-network-stack.md`; the
fork commit is on socket2's `toyos` branch.
