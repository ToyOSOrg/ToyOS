---
status: open
kind: defect
opened: 2026-09-26
---

# A netstack client cannot tell a reset connection from the peer's FIN

A stream reaches its client as two pipes, and the receive pipe's end says only
that it ended. std (`ended`, `sdk/std/sys/net/connection.rs`) and libc
(`userland/libc/src/streamend.rs`) read the kind off the send pipe: one with no
reader behind a receive pipe at its end is a failure.

On the node that order holds: a failure lets the send pipe go before the
receive pipe, and an orderly end keeps a send pipe whose last byte and FIN are
queued until its writer leaves (`userland/netstack/node/src/streams.rs`'s
header; its host tests in `userland/netstack/node/tests/streams.rs`, each red
with the order broken).

What `main` ships is netstack on smoltcp, whose `bridge_piped`
(`userland/netstack/src/main.rs`) closes the receive pipe and then, in the same
pass, the send pipe of a connection that is no longer open, whether it failed
or ended in order. A client that reads the end between the two reads a
failure as a FIN, and one that reads it after both reads a FIN that came with
both directions done in one pass as a failure. Measured in a guest on `main`,
against the host kernel's TCP behind QEMU's user network: a reset mid-stream,
a reset after the client's half-close, a FIN before the client's write and a
FIN after which the client shut its sending half each read as a host's TCP
reads them, in each of two runs; neither met the race.

**Exit condition**: netstack runs on the node, and a guest test whose client
shuts its sending half down reads its peer's bytes and then the end as an end,
one whose peer resets a stream reads the reset as a reset, and each line it
prints is the host kernel's for the same program against the same peer.

**Owner**: whoever holds `issues/toyos-has-its-own-network-stack.md`.
