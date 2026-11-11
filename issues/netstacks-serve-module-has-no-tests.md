---
status: open
kind: tooling
opened: 2026-11-11
---

# netstack's serve module has no tests

`netstack_socket_churn` was cut as an integration test of a userland program.
What it reached of the node is held by `system/netstack/node/tests/`; what only
`system/netstack/src/serve.rs` does, no test reaches.

**Owner**: whoever holds `issues/toyos-has-its-own-network-stack.md`.

**Exit condition**: `system/netstack/src/serve.rs` has its own tests of a
second receive refused `ERR_RESOURCE_EXHAUSTED`, a waiting receive answered by
its datagram (`settle`), and the kernel's watch answers that end a stream, a
listener or a datagram socket whose owner left or whose end is no pipe
(`Sockets::answered`).
