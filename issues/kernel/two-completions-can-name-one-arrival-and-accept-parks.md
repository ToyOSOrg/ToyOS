---
status: open
kind: defect
opened: 2026-09-27
---

# Two completions can name one arrival, and an `accept` after the second parks

`process_watch` (`kernel/src/inbox/mod.rs`) answers a watch on a handle that
is already ready at once, and only a watch it has to register replaces the
armed one on the same handle. So a watch submitted while a connection arrives
can complete immediately beside the older armed watch that the same arrival
fires: two completions for one connection, in one drain or in two. A read
after a spurious completion is harmless where the read does not block, but
`SYS_ACCEPT` (`kernel/src/syscall/ipc.rs`, `sys_accept`) parks until a
connection is queued, so a server that accepts once per completion takes the
connection on the first and parks for good on the second — the port it serves
then answers nobody.

Measured on `/system/bin/fsd`'s log server under `tests/quiescecase`, whose six
writers connect beside logd at boot: a blocked-task dump showed the server
`ipc parked` (the class only `sys_accept` parks in) while logd and every writer
waited on it, in one boot of four and in one of three in a second series. fsd
now asks the acceptor with a zero-timeout watch on a probe ring before it
accepts (`userland/fsd/src/main.rs`, `Server::accept`), and after the change
the same boot ran six times and never parked. Every other server that accepts after
a completion — blockd, logd's inspect and serve threads, init,
soundd, the compositor, netd, filepicker — does not.

**Exit**: a spurious completion cannot park a server — an accept that does not
park when nothing is queued, or a watch that withdraws the armed one on its
handle whether or not it answers at once, with a test that submits a watch
while a connection arrives and counts the completions.
