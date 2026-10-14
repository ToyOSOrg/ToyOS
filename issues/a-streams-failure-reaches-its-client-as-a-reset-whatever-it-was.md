---
status: open
kind: defect
opened: 2026-10-09
---

# A stream's failure is told its client by name, and only a reset is tested

The receive ring's `end` word carries which failure ended a stream:
`RESET`, `TIMED_OUT` or `UNREACHABLE` (`sdk/toyos/src/net/stream.rs`), which
the node stores from [tcp]'s `Failure` (`system/netstack/node/src/streams.rs`),
std answers `ConnectionReset`, `TimedOut` or `HostUnreachable`
(`sdk/std/sys/net/connection.rs`) and libc `ECONNRESET`, `ETIMEDOUT` or
`EHOSTUNREACH` (`sdk/libc/src/socket.rs`). The node's host tests read a
peer's reset as `RESET`; nothing reads the other two words, and no guest test
reads any of them through std or libc.

**Exit condition**: a node host test reads a connection [tcp] gave up on as
`TIMED_OUT` and one an ICMP error ended as `UNREACHABLE`, and a guest test
whose peer is unreachable reads it as unreachable through std.

**Owner**: whoever holds `issues/toyos-has-its-own-network-stack.md`.
