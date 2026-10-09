---
status: open
kind: defect
opened: 2026-10-09
---

# A stream's failure reaches its client as a reset, whatever it was

The pipe ABI tells a stream's client how the stream ended by the order its two
pipes end in (`userland/netstack/node/src/streams.rs`'s header), which carries
one bit: the peer's FIN, or a failure. The node knows which failure
(`toyos_net_tcp::Failure`: a reset, R2's timeout, an ICMP error), and std and
libc answer every one `ConnectionReset` and `ECONNRESET`, where Linux answers a
connection [tcp] gave up on `ETIMEDOUT` and one an ICMP error ended
`EHOSTUNREACH` or `ENETUNREACH`.

A pipe's end carries no word, and every other carrier costs what this one does
not: a third pipe or a kept request connection is a 2 MiB page each
(`kernel/src/pipe.rs`, `PIPE_SIZE`), and a reason the node keeps for a client to
ask about after both pipes are gone is kept for ever for a client that died,
since nothing then tells the node it left. The clean carrier is a word the
writer of a pipe sets as it lets its end go and the reader reads at the end,
which is the kernel's to add.

The same word set would answer a connect that an ICMP error ended, which the
move's netstack answers `ERR_OTHER` for want of a word.

Read from the code, not measured.

**Exit condition**: a pipe's end carries its reason, the node writes
`Failure`'s through it, and std and libc answer each as Linux does; a node
host test reads each failure's word, and a guest test whose peer is
unreachable reads it as unreachable.

**Owner**: whoever holds `issues/toyos-has-its-own-network-stack.md`.
